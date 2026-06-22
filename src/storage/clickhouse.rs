use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use chrono::{DateTime, TimeZone, Utc};
use clickhouse::{Client, Row, query::Query};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::types::{
    DashboardMetrics, DashboardSparkBucket, FleetRibbonBucket, LatencyBucket, PriorPeriodSummary,
    RegionLatencySeries, RegionRollup, StatusBreakdown,
};
use crate::config::ClickhouseConfig;
use crate::domain::{CheckResult, CheckStatus, Incident, OrgId, coalesce_incidents};
use crate::error::Result;
use crate::storage::org_ttl::OrgTtlDays;
use crate::storage::traits::{ClampedRange, ResultSink, ResultsStore, TimeRange, UptimeStats};

const TABLE: &str = "check_results";

/// Seconds bound for the matview `minute` column (`DateTime`).
#[derive(Serialize)]
struct MinuteBound(u32);

impl MinuteBound {
    fn new(dt: DateTime<Utc>) -> Self {
        Self(to_unix_secs(dt))
    }
}

/// `timestamp`/`ingested_at` are `DateTime` (UInt32 seconds on the wire).
fn to_unix_secs(dt: DateTime<Utc>) -> u32 {
    dt.timestamp().clamp(0, i64::from(u32::MAX)) as u32
}

fn from_unix_secs(secs: u32) -> DateTime<Utc> {
    Utc.timestamp_opt(i64::from(secs), 0)
        .single()
        .unwrap_or_else(Utc::now)
}

/// `[from, to)` over the matview `minute` column; bind via [`bind_minute_window`].
const MINUTE_WINDOW: &str = "minute >= fromUnixTimestamp(?) AND minute < fromUnixTimestamp(?)";

/// 1m rollup TTL (days). Ranges reaching past it read the 1h rollup instead.
const MINUTE_ROLLUP_DAYS: i64 = 30;

/// The two [`MinuteBound`] binds [`MINUTE_WINDOW`] expects; call after the
/// leading positional binds (org_id, target_id, …).
fn bind_minute_window(q: Query, from: DateTime<Utc>, to: DateTime<Utc>) -> Query {
    q.bind(MinuteBound::new(from)).bind(MinuteBound::new(to))
}

/// `(table, time_column)` for a range: the 1m rollup within its TTL, else the
/// 1h rollup. Both carry the same AggregateFunction columns, so a merge query
/// reads either by swapping these two tokens.
fn rollup_source(from: DateTime<Utc>) -> (&'static str, &'static str) {
    if from >= Utc::now() - chrono::Duration::days(MINUTE_ROLLUP_DAYS) {
        ("check_results_1m", "minute")
    } else {
        ("check_results_1h", "hour")
    }
}

/// Ordered list of migrations. Each entry is `(filename, sql)`. Filename is
/// recorded in `schema_migrations` after apply so we never re-run a migration
/// that has already executed on this database.
///
/// Crash-atomicity discipline: every migration MUST be idempotent on re-run
/// (i.e. only `CREATE … IF NOT EXISTS` / `ALTER … IF EXISTS`). ClickHouse has
/// no transactions across DDL statements and `schema_migrations` is TinyLog
/// (no atomic CAS), so the apply-then-record sequence is not crash-atomic:
/// an OOM-kill between the last statement and the recording INSERT leaves
/// the migration officially un-applied and the next boot re-runs it. A
/// destructive statement (DROP/TRUNCATE) under those conditions wipes live
/// data, which is why migrations contain none.
///
/// The splitter is a real tokenizer ([`split_statements`]) — it tracks
/// single/double-quote string literals, backtick identifiers, line
/// comments (`--`) and block comments (`/* … */`), so `;` inside any of
/// those does **not** become a chunk boundary. CREATE FUNCTION bodies,
/// regex defaults containing `';'`, doubled-quote escapes (`'it''s'`) and
/// backslash escapes (`'a\\'b'`) all round-trip.
///
/// The runner is not concurrent-safe: two processes racing through their
/// first boot could both observe an empty applied set and both run the
/// migration. With the IF NOT EXISTS discipline above this is harmless
/// (second CREATE is a no-op); for multi-replica, take a pg_advisory_lock
/// around the call.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "001_initial.sql",
        include_str!("../../migrations/clickhouse/001_initial.sql"),
    ),
    // 002 `check_results_1h`: the hour-rollup for the long history tail. Same
    // AggregateFunction columns as `check_results_1m`, so [`rollup_source`]
    // merges either with one finaliser set; it routes ranges past the 1m
    // rollup's 30-day TTL here. A 2nd matview on raw `check_results` (accrues
    // forward, no backfill). The 13-month TTL exceeds the raw/1m TTL, so the
    // Privacy Policy and the `retention_test` guard disclose it; org erasure
    // must clear it too (see [`CH_TENANT_TABLES`]). Migration SQL is frozen —
    // keep this rationale here.
    (
        "002_check_results_1h.sql",
        include_str!("../../migrations/clickhouse/002_check_results_1h.sql"),
    ),
];

pub async fn migrate(client: &Client) -> Result<()> {
    tracing::info!("running clickhouse migrations");

    client
        .query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
                 filename String, \
                 applied_at DateTime64(3, 'UTC') DEFAULT now64(3) \
             ) ENGINE = TinyLog",
        )
        .execute()
        .await
        .context("create schema_migrations")?;

    #[derive(Row, Deserialize)]
    struct AppliedRow {
        filename: String,
    }
    let applied: Vec<AppliedRow> = client
        .query("SELECT filename FROM schema_migrations")
        .fetch_all::<AppliedRow>()
        .await
        .context("read schema_migrations")?;

    for (name, sql) in MIGRATIONS {
        if applied.iter().any(|f| f.filename == *name) {
            tracing::debug!(migration = name, "clickhouse migration already applied");
            continue;
        }
        tracing::info!(migration = name, "applying clickhouse migration");
        for stmt in split_statements(sql)
            .with_context(|| format!("clickhouse migration {name}: tokenize source"))?
        {
            client
                .query(&stmt)
                .execute()
                .await
                .with_context(|| format!("clickhouse migration {name}"))?;
        }
        client
            .query("INSERT INTO schema_migrations (filename) VALUES (?)")
            .bind(*name)
            .execute()
            .await
            .with_context(|| format!("record clickhouse migration {name}"))?;

        // Fence: re-read schema_migrations and confirm the row landed. Without
        // this an INSERT that the server accepted but failed to persist (TinyLog
        // gives no fsync guarantee, a crash mid-flush can lose the row) would
        // let the next boot re-run the migration. With the IF-NOT-EXISTS
        // discipline above that is harmless today, but the fence costs one
        // count query per migration and removes the silent footgun outright.
        let CountRow { n } = client
            .query("SELECT count() AS n FROM schema_migrations WHERE filename = ?")
            .bind(*name)
            .fetch_one::<CountRow>()
            .await
            .with_context(|| format!("verify clickhouse migration recorded {name}"))?;
        if n == 0 {
            return Err(anyhow::anyhow!(
                "clickhouse migration {name} applied but not recorded in schema_migrations",
            )
            .into());
        }
    }

    verify_rollup_schema(client).await?;

    tracing::info!("clickhouse ready");
    Ok(())
}

/// Exact `(column, type)` shape of `check_results_1m`, in definition order.
/// Editing the matview in `001_initial.sql` is a no-op on an existing DB
/// (recorded migration + `IF NOT EXISTS`), so [`verify_rollup_schema`] checks
/// the live view against this at boot and fails loud on drift. A matview change
/// = recreate migration + update 001 + update this list. Type strings are
/// CH-version-formatted; a server upgrade that reformats them fails boot here.
const EXPECTED_ROLLUP_SCHEMA: &[(&str, &str)] = &[
    ("org_id", "UUID"),
    ("target_id", "UUID"),
    ("region", "LowCardinality(String)"),
    ("minute", "DateTime('UTC')"),
    ("total_checks", "AggregateFunction(count)"),
    ("up_checks", "AggregateFunction(countIf, UInt8)"),
    ("down_checks", "AggregateFunction(countIf, UInt8)"),
    ("degraded_checks", "AggregateFunction(countIf, UInt8)"),
    ("error_checks", "AggregateFunction(countIf, UInt8)"),
    ("avg_duration_ms", "AggregateFunction(avg, UInt32)"),
    (
        "duration_quantiles",
        "AggregateFunction(quantiles(0.5, 0.95, 0.99), UInt32)",
    ),
    ("avg_dns_ms", "AggregateFunction(avg, Nullable(UInt16))"),
    ("avg_connect_ms", "AggregateFunction(avg, Nullable(UInt16))"),
    ("avg_tls_ms", "AggregateFunction(avg, Nullable(UInt16))"),
    ("avg_ttfb_ms", "AggregateFunction(avg, Nullable(UInt16))"),
    (
        "last_status_state",
        "AggregateFunction(argMax, Enum8('up' = 1, 'down' = 2, 'degraded' = 3, 'error' = 4), DateTime('UTC'))",
    ),
];

/// Boot check: both rollups must equal [`EXPECTED_ROLLUP_SCHEMA`]. The 1h view
/// mirrors the 1m column set with `hour` in place of `minute`.
async fn verify_rollup_schema(client: &Client) -> Result<()> {
    verify_view_schema(client, "check_results_1m", EXPECTED_ROLLUP_SCHEMA).await?;
    let expected_1h: Vec<(&str, &str)> = EXPECTED_ROLLUP_SCHEMA
        .iter()
        .map(|(n, t)| {
            if *n == "minute" {
                ("hour", *t)
            } else {
                (*n, *t)
            }
        })
        .collect();
    verify_view_schema(client, "check_results_1h", &expected_1h).await?;
    Ok(())
}

async fn verify_view_schema(client: &Client, view: &str, expected: &[(&str, &str)]) -> Result<()> {
    #[derive(Row, Deserialize)]
    struct Col {
        name: String,
        #[serde(rename = "type")]
        ty: String,
    }
    let live: Vec<(String, String)> = client
        .query(
            "SELECT name, type FROM system.columns \
             WHERE database = currentDatabase() AND table = ? \
             ORDER BY position",
        )
        .bind(view)
        .fetch_all::<Col>()
        .await
        .context("clickhouse verify_view_schema: read system.columns")?
        .into_iter()
        .map(|c| (c.name, c.ty))
        .collect();
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(n, t)| ((*n).to_string(), (*t).to_string()))
        .collect();
    if live != expected {
        return Err(anyhow::anyhow!(
            "{view} schema drifted from the readers' contract — a matview edit is a \
             no-op on an existing DB; ship a recreate migration and update \
             EXPECTED_ROLLUP_SCHEMA.\n  expected: {expected:?}\n  live:     {live:?}"
        )
        .into());
    }
    Ok(())
}

/// Split a migration source into executable statements with full
/// awareness of string literals and comments.
///
/// Quoted regions (`'…'`, `"…"`, `` `…` ``) and comments (`--` to
/// newline, `/* … */` block) suppress `;` recognition. Doubled quotes
/// (`''`, `""`) and backslash escapes (`\'`, `\\`) inside a string keep
/// the parser inside that string. Comment bodies are dropped from the
/// emitted statement; quoted bodies are kept verbatim.
///
/// Returns an error on an unterminated string literal or block comment
/// at EOF — these are migration bugs that must boot-fail loudly with a
/// pointer to the source rather than be papered over with a half-parsed
/// statement that ClickHouse then rejects far from the cause.
fn split_statements(sql: &str) -> Result<Vec<String>> {
    enum State {
        Normal,
        Quoted(char),
        LineComment,
        BlockComment,
    }

    let mut state = State::Normal;
    let mut current = String::new();
    let mut out = Vec::new();
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        match state {
            State::Normal => {
                if c == ';' {
                    push_statement(&mut current, &mut out);
                } else if c == '-' && chars.peek() == Some(&'-') {
                    chars.next();
                    state = State::LineComment;
                } else if c == '/' && chars.peek() == Some(&'*') {
                    chars.next();
                    state = State::BlockComment;
                } else if c == '\'' || c == '"' || c == '`' {
                    current.push(c);
                    state = State::Quoted(c);
                } else {
                    current.push(c);
                }
            }
            State::Quoted(q) => {
                current.push(c);
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if c == q {
                    if chars.peek() == Some(&q) {
                        if let Some(next) = chars.next() {
                            current.push(next);
                        }
                    } else {
                        state = State::Normal;
                    }
                }
            }
            State::LineComment => {
                if c == '\n' {
                    current.push('\n');
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = State::Normal;
                }
            }
        }
    }
    match state {
        State::Normal | State::LineComment => {
            push_statement(&mut current, &mut out);
            Ok(out)
        }
        State::Quoted(q) => {
            Err(anyhow::anyhow!("unterminated {q} string literal in migration source").into())
        }
        State::BlockComment => {
            Err(anyhow::anyhow!("unterminated /* … */ block comment in migration source").into())
        }
    }
}

fn push_statement(buf: &mut String, out: &mut Vec<String>) {
    let trimmed = buf.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    buf.clear();
}

pub fn build_client(cfg: &ClickhouseConfig) -> Client {
    let mut client = Client::default()
        .with_url(&cfg.url)
        .with_database(&cfg.database)
        .with_user(&cfg.user);
    if !cfg.password.expose_secret().is_empty() {
        client = client.with_password(cfg.password.expose_secret());
    }
    if cfg.async_insert {
        // INSERT-only settings; SELECTs ignore them. `wait_for_async_insert`
        // keeps end() returning only after a durable flush. `async_insert`
        // coalesces server-side, which forms its own blocks — so the raw
        // MergeTree block dedup no longer recognises a re-sent batch;
        // `async_insert_deduplicate` restores idempotency for the batcher's
        // identical-block retry.
        client = client
            .with_setting("async_insert", "1")
            .with_setting("wait_for_async_insert", "1")
            .with_setting("async_insert_deduplicate", "1");
    }
    client
}

/// Unbounded live row count for one org in a single ClickHouse table. Used by
/// the GDPR purge to prove an erasure actually landed (zero == erased), so it
/// deliberately takes no time range — it must see *every* surviving row.
/// `table` is always a fixed in-crate constant at the call sites, never user
/// input, so the identifier interpolation is injection-free.
pub(crate) async fn count_org_rows(client: &Client, table: &str, org_id: Uuid) -> Result<u64> {
    let row: CountRow = client
        .query(&format!(
            "SELECT count() AS n FROM {table} WHERE org_id = ?"
        ))
        .bind(org_id)
        .fetch_one::<CountRow>()
        .await
        .with_context(|| format!("clickhouse count_org_rows {table}"))?;
    Ok(row.n)
}

/// Single-column scalar count row, shared by every `SELECT count() AS n …`
/// call in this module so each callsite doesn't redefine its own struct.
#[derive(Row, Deserialize)]
struct CountRow {
    n: u64,
}

pub struct ClickhouseResultSink {
    client: Client,
    /// Region + agent id stamped on results from this control plane's own
    /// scheduler. Agent-submitted batches carry their own via [`write_batch_tagged`].
    region: String,
    agent_id: String,
    /// Per-org physical retention, resolved from the org's plan at write time.
    org_ttl: OrgTtlDays,
}

impl ClickhouseResultSink {
    pub fn new(client: Client, region: String, agent_id: String, org_ttl: OrgTtlDays) -> Self {
        Self {
            client,
            region,
            agent_id,
            org_ttl,
        }
    }

    async fn write_once(
        &self,
        rows: &[CheckResultRow<'_>],
    ) -> std::result::Result<(), clickhouse::error::Error> {
        let mut insert = self.client.insert::<CheckResultRow>(TABLE).await?;
        for row in rows {
            insert.write(row).await?;
        }
        insert.end().await
    }

    async fn write_batch_inner(
        &self,
        results: &[CheckResult],
        region: &str,
        agent_id: &str,
    ) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        let ttls = self.org_ttl.days_for_each(results.iter().map(|r| r.org_id));
        let rows: Vec<CheckResultRow<'_>> = results
            .iter()
            .zip(ttls)
            .map(|(r, ttl)| CheckResultRow::from_result(r, region, agent_id, ttl))
            .collect();

        let backoff = ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(100))
            .with_max_interval(Duration::from_secs(5))
            .with_max_elapsed_time(Some(Duration::from_secs(30)))
            .build();

        // Full re-send on each retry is intentional and safe ONLY because the
        // `check_results` table sets `non_replicated_deduplication_window` (see
        // 001_initial.sql): a commit-then-lost-ack retry re-sends the identical
        // block, which the server drops by hash instead of double-counting.
        // Don't checkpoint mid-batch here — a partial re-send changes the block
        // and defeats that dedup.
        let op = || async {
            self.write_once(&rows)
                .await
                .map_err(backoff::Error::transient)
        };

        if let Err(err) = backoff::future::retry(backoff, op).await {
            tracing::error!(
                ?err,
                count = rows.len(),
                "clickhouse insert failed after retries"
            );
            return Err(anyhow::anyhow!("clickhouse insert failed: {err}").into());
        }
        Ok(())
    }
}

#[async_trait]
impl ResultSink for ClickhouseResultSink {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()> {
        self.write_batch_inner(results, &self.region, &self.agent_id)
            .await
    }

    async fn write_batch_tagged(
        &self,
        results: &[CheckResult],
        region: &str,
        agent_id: &str,
    ) -> Result<()> {
        self.write_batch_inner(results, region, agent_id).await
    }
}

#[derive(Debug, Row, Serialize)]
struct CheckResultRow<'a> {
    #[serde(with = "clickhouse::serde::uuid")]
    org_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    // Sent explicitly, not via column DEFAULT: the matview groups on the
    // inserted block, so `region` must be in it.
    region: &'a str,
    timestamp: u32,
    agent_id: &'a str,
    status: i8,
    duration_ms: u32,
    dns_ms: Option<u16>,
    connect_ms: Option<u16>,
    tls_ms: Option<u16>,
    ttfb_ms: Option<u16>,
    response_code: Option<u16>,
    response_size: Option<u32>,
    error: Option<&'a str>,
    ttl_days: u16,
}

impl<'a> CheckResultRow<'a> {
    fn from_result(r: &'a CheckResult, region: &'a str, agent_id: &'a str, ttl_days: u16) -> Self {
        Self {
            org_id: r.org_id,
            target_id: r.target_id,
            region,
            timestamp: to_unix_secs(r.timestamp),
            agent_id,
            status: r.status.as_enum8(),
            duration_ms: r.duration_ms,
            dns_ms: r.dns_ms,
            connect_ms: r.connect_ms,
            tls_ms: r.tls_ms,
            ttfb_ms: r.ttfb_ms,
            response_code: r.response_code,
            response_size: r.response_size,
            error: r.error.as_deref(),
            ttl_days,
        }
    }
}

/// Org-scoped read side of the ClickHouse results table. Holds no ambient
/// org; every query binds the `org` the caller resolved from the request, so
/// a `target_id` guessed from another tenant returns zero rows. `org_id` is
/// also the leading sort key — filtering on it is mandatory for performance,
/// not only isolation.
pub struct ClickhouseResultsStore {
    client: Client,
}

impl ClickhouseResultsStore {
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Debug, Row, Deserialize)]
struct IncidentRow {
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    timestamp: u32,
    status: i8,
    error: Option<String>,
}

fn coalesce_from_incident_rows(target_id: Uuid, rows: Vec<IncidentRow>) -> Vec<Incident> {
    coalesce_incidents(
        target_id,
        rows.into_iter().map(|r| {
            (
                from_unix_secs(r.timestamp),
                CheckStatus::from_enum8(r.status),
                r.error,
            )
        }),
    )
}

#[derive(Debug, Row, Deserialize)]
struct OwnedResultRow {
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    timestamp: u32,
    status: i8,
    duration_ms: u32,
    dns_ms: Option<u16>,
    connect_ms: Option<u16>,
    tls_ms: Option<u16>,
    ttfb_ms: Option<u16>,
    response_code: Option<u16>,
    response_size: Option<u32>,
    error: Option<String>,
}

#[derive(Debug, Row, Deserialize)]
struct RegionResultRow {
    region: String,
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    timestamp: u32,
    status: i8,
    duration_ms: u32,
    dns_ms: Option<u16>,
    connect_ms: Option<u16>,
    tls_ms: Option<u16>,
    ttfb_ms: Option<u16>,
    response_code: Option<u16>,
    response_size: Option<u32>,
    error: Option<String>,
}

impl RegionResultRow {
    fn split(self, org_id: Uuid) -> (String, CheckResult) {
        let inner = OwnedResultRow {
            target_id: self.target_id,
            timestamp: self.timestamp,
            status: self.status,
            duration_ms: self.duration_ms,
            dns_ms: self.dns_ms,
            connect_ms: self.connect_ms,
            tls_ms: self.tls_ms,
            ttfb_ms: self.ttfb_ms,
            response_code: self.response_code,
            response_size: self.response_size,
            error: self.error,
        };
        (self.region, row_to_result(inner, org_id))
    }
}

fn row_to_result(row: OwnedResultRow, org_id: Uuid) -> CheckResult {
    CheckResult {
        target_id: row.target_id,
        org_id,
        timestamp: from_unix_secs(row.timestamp),
        status: CheckStatus::from_enum8(row.status),
        duration_ms: row.duration_ms,
        dns_ms: row.dns_ms,
        connect_ms: row.connect_ms,
        tls_ms: row.tls_ms,
        ttfb_ms: row.ttfb_ms,
        response_code: row.response_code,
        response_size: row.response_size,
        error: row.error,
    }
}

#[async_trait]
impl ResultsStore for ClickhouseResultsStore {
    async fn ping(&self) -> Result<()> {
        self.client
            .query("SELECT 1")
            .execute()
            .await
            .context("clickhouse ping")?;
        Ok(())
    }

    async fn list_results(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        limit: usize,
        offset: usize,
        region: Option<&str>,
    ) -> Result<Vec<CheckResult>> {
        let limit = limit.min(10_000) as u64;
        let offset = offset as u64;
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT target_id, timestamp, status, duration_ms, dns_ms, connect_ms, tls_ms, \
                 ttfb_ms, response_code, response_size, error FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?) \
                 ORDER BY timestamp DESC LIMIT ? OFFSET ?"
            ))
            .bind(org.0)
            .bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<OwnedResultRow> = q
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .bind(limit)
            .bind(offset)
            .fetch_all::<OwnedResultRow>()
            .await
            .context("clickhouse list_results")?;
        Ok(rows.into_iter().map(|r| row_to_result(r, org.0)).collect())
    }

    async fn list_results_by_region(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(String, CheckResult)>> {
        let limit = limit.min(10_000) as u64;
        let offset = offset as u64;
        let rows: Vec<RegionResultRow> = self
            .client
            .query(&format!(
                "SELECT region, target_id, timestamp, status, duration_ms, dns_ms, connect_ms, \
                 tls_ms, ttfb_ms, response_code, response_size, error FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?) \
                 ORDER BY timestamp DESC LIMIT ? OFFSET ?"
            ))
            .bind(org.0)
            .bind(target_id)
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .bind(limit)
            .bind(offset)
            .fetch_all::<RegionResultRow>()
            .await
            .context("clickhouse list_results_by_region")?;
        Ok(rows.into_iter().map(|r| r.split(org.0)).collect())
    }

    async fn current_status_breakdown(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<StatusBreakdown> {
        #[derive(Row, Deserialize)]
        struct Latest {
            status: i8,
        }
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT argMax(status, timestamp) AS status \
                 FROM {TABLE} \
                 WHERE org_id = ? {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?) \
                 GROUP BY target_id"
            ))
            .bind(org.0);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<Latest> = q
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .fetch_all::<Latest>()
            .await
            .context("clickhouse current_status_breakdown")?;
        let mut out = StatusBreakdown::default();
        for r in rows {
            match CheckStatus::from_enum8(r.status) {
                CheckStatus::Up => out.up += 1,
                CheckStatus::Down => out.down += 1,
                CheckStatus::Degraded => out.degraded += 1,
                CheckStatus::Error => out.error += 1,
            }
        }
        Ok(out)
    }

    async fn last_n_summary(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<(u64, u64, u32, u64)> {
        #[derive(Row, Deserialize)]
        struct Counts {
            total: u64,
            up: u64,
            avg_ms: f64,
        }
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut cq = self
            .client
            .query(&format!(
                "SELECT count() AS total, countIf(status = 'up') AS up, \
                        avg(duration_ms) AS avg_ms \
                 FROM {TABLE} \
                 WHERE org_id = ? {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?)"
            ))
            .bind(org.0);
        if let Some(r) = region {
            cq = cq.bind(r);
        }
        let counts: Counts = cq
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .fetch_one::<Counts>()
            .await
            .context("clickhouse last_n_summary counts")?;

        // Pull the entire range in one query ordered by (target_id, timestamp).
        // Coalesce per-target client-side; avoids one round-trip per target.
        let mut rq = self
            .client
            .query(&format!(
                "SELECT target_id, timestamp, status, error FROM {TABLE} \
                 WHERE org_id = ? {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?) \
                 ORDER BY target_id ASC, timestamp ASC"
            ))
            .bind(org.0);
        if let Some(r) = region {
            rq = rq.bind(r);
        }
        let rows: Vec<IncidentRow> = rq
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .fetch_all::<IncidentRow>()
            .await
            .context("clickhouse last_n_summary rows")?;

        let mut incidents = 0u64;
        let mut group: Vec<IncidentRow> = Vec::new();
        let mut current_target: Option<Uuid> = None;
        for row in rows {
            match current_target {
                Some(t) if t == row.target_id => group.push(row),
                _ => {
                    if let Some(t) = current_target.take() {
                        incidents +=
                            coalesce_from_incident_rows(t, std::mem::take(&mut group)).len() as u64;
                    }
                    current_target = Some(row.target_id);
                    group.push(row);
                }
            }
        }
        if let Some(t) = current_target {
            incidents += coalesce_from_incident_rows(t, group).len() as u64;
        }
        let avg_ms = if counts.avg_ms.is_finite() {
            counts.avg_ms.round().clamp(0.0, u32::MAX as f64) as u32
        } else {
            0
        };
        Ok((counts.total, counts.up, avg_ms, incidents))
    }

    async fn uptime(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        region: Option<&str>,
    ) -> Result<UptimeStats> {
        #[derive(Row, Deserialize)]
        struct CountsRow {
            up: u64,
            down: u64,
            degraded: u64,
            error: u64,
        }

        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT \
                   countIf(status = 'up') AS up, \
                   countIf(status = 'down') AS down, \
                   countIf(status = 'degraded') AS degraded, \
                   countIf(status = 'error') AS error \
                 FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?)"
            ))
            .bind(org.0)
            .bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let row: CountsRow = q
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .fetch_one::<CountsRow>()
            .await
            .context("clickhouse uptime")?;

        let total = row.up + row.down + row.degraded + row.error;
        let uptime_pct = if total > 0 {
            (row.up as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        Ok(UptimeStats {
            total,
            up: row.up,
            down: row.down,
            degraded: row.degraded,
            error: row.error,
            uptime_pct,
        })
    }

    async fn dashboard_rollup(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<Vec<DashboardMetrics>> {
        // Merges from the per-minute matview, not raw `check_results`.
        // `minute` is `DateTime` (UInt32 s), so the bind is u32-seconds.
        #[derive(Row, Deserialize)]
        struct RollupRow {
            #[serde(with = "clickhouse::serde::uuid")]
            target_id: Uuid,
            samples: u64,
            up: u64,
            avg_ms: f64,
            quantiles: Vec<f64>,
            last_status: i8,
            last_minute_ts: u32,
        }
        let (table, tcol) = rollup_source(range.from);
        let query = format!(
            "SELECT \
               target_id, \
               countMerge(total_checks) AS samples, \
               countIfMerge(up_checks) AS up, \
               avgMerge(avg_duration_ms) AS avg_ms, \
               quantilesMerge(0.5, 0.95)(duration_quantiles) AS quantiles, \
               argMaxMerge(last_status_state) AS last_status, \
               toUInt32(max({tcol})) AS last_minute_ts \
             FROM {table} \
             WHERE org_id = ? {region_pred} \
               AND {tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?) \
             GROUP BY target_id",
            region_pred = region.map(|_| "AND region = ?").unwrap_or("")
        );
        let mut q = self.client.query(&query).bind(org.0);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<RollupRow> = bind_minute_window(q, range.from, range.to)
            .fetch_all::<RollupRow>()
            .await
            .context("clickhouse dashboard_rollup")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let p50 = r.quantiles.first().copied().unwrap_or(0.0);
                let p95 = r.quantiles.get(1).copied().unwrap_or(0.0);
                let avg = if r.avg_ms.is_finite() { r.avg_ms } else { 0.0 };
                DashboardMetrics {
                    target_id: r.target_id,
                    samples: r.samples,
                    up: r.up,
                    avg_ms: avg.round().clamp(0.0, u32::MAX as f64) as u32,
                    p50_ms: p50.round().clamp(0.0, u32::MAX as f64) as u32,
                    p95_ms: p95.round().clamp(0.0, u32::MAX as f64) as u32,
                    last_status: CheckStatus::from_enum8(r.last_status).as_str().to_string(),
                    last_minute_ts: (r.last_minute_ts > 0).then_some(r.last_minute_ts as i64),
                }
            })
            .collect())
    }

    async fn dashboard_sparkline(
        &self,
        org: OrgId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        region: Option<&str>,
    ) -> Result<Vec<DashboardSparkBucket>> {
        // Reads the per-minute rollup so cost stays O(buckets), not O(raw). The
        // `avgState` column needs its `avgMerge` finaliser in `SELECT`.
        #[derive(Row, Deserialize)]
        struct SparkRow {
            #[serde(with = "clickhouse::serde::uuid")]
            target_id: Uuid,
            bucket_ts: u32,
            avg_ms: f64,
        }
        let query = format!(
            "SELECT \
               target_id, \
               toUInt32(minute) AS bucket_ts, \
               avgMerge(avg_duration_ms) AS avg_ms \
             FROM check_results_1m \
             WHERE org_id = ? {region_pred} AND {MINUTE_WINDOW} \
             GROUP BY target_id, minute \
             ORDER BY target_id, minute",
            region_pred = region.map(|_| "AND region = ?").unwrap_or("")
        );
        let mut q = self.client.query(&query).bind(org.0);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<SparkRow> = bind_minute_window(q, from, to)
            .fetch_all::<SparkRow>()
            .await
            .context("clickhouse dashboard_sparkline")?;
        Ok(rows
            .into_iter()
            .map(|r| DashboardSparkBucket {
                target_id: r.target_id,
                bucket_ts: r.bucket_ts as i64,
                avg_ms: r.avg_ms as f32,
            })
            .collect())
    }

    async fn latency_buckets(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
        region: Option<&str>,
    ) -> Result<Vec<LatencyBucket>> {
        #[derive(Row, Deserialize)]
        struct LatRow {
            bucket_ts: u32,
            // quantilesMerge(0.5, 0.95, 0.99) finalises to Array(Float64) of
            // length 3, in the level order requested.
            quantiles: Vec<f64>,
            avg: f64,
            dns: f64,
            connect: f64,
            tls: f64,
            ttfb: f64,
            samples: u64,
        }
        // Round up to a whole number of source rows so every output bucket
        // spans an integer count of minutes.
        let bucket = bucket_seconds.max(60).div_ceil(60) * 60;
        // `minute`/`hour` are both DateTime seconds, so `bind_minute_window`
        // binds either.
        let (table, tcol) = rollup_source(range.from);
        let window = format!("{tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?)");
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let query = format!(
            "SELECT \
               toUInt32(toStartOfInterval({tcol}, INTERVAL {bucket} SECOND)) AS bucket_ts, \
               quantilesMerge(0.5, 0.95, 0.99)(duration_quantiles) AS quantiles, \
               avgMerge(avg_duration_ms) AS avg, \
               ifNull(avgMerge(avg_dns_ms), 0) AS dns, \
               ifNull(avgMerge(avg_connect_ms), 0) AS connect, \
               ifNull(avgMerge(avg_tls_ms), 0) AS tls, \
               ifNull(avgMerge(avg_ttfb_ms), 0) AS ttfb, \
               countMerge(total_checks) AS samples \
             FROM {table} \
             WHERE org_id = ? AND target_id = ? {region_pred} AND {window} \
             GROUP BY bucket_ts \
             ORDER BY bucket_ts"
        );
        let mut q = self.client.query(&query).bind(org.0).bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<LatRow> = bind_minute_window(q, range.from, range.to)
            .fetch_all::<LatRow>()
            .await
            .context("clickhouse latency_buckets")?;
        // Phases are folded to 0 in SQL via ifNull, and every emitted bucket
        // has >= 1 sample, so each merged mean is a finite non-negative float.
        let ms = |v: f64| v.round().max(0.0) as u32;
        let buckets: Vec<LatencyBucket> = rows
            .into_iter()
            .map(|r| {
                let q = |i: usize| r.quantiles.get(i).copied().map(ms).unwrap_or(0);
                LatencyBucket {
                    t: i64::from(r.bucket_ts) * 1000,
                    p50: q(0),
                    p95: q(1),
                    p99: q(2),
                    avg: ms(r.avg),
                    dns: ms(r.dns),
                    connect: ms(r.connect),
                    tls: ms(r.tls),
                    ttfb: ms(r.ttfb),
                    samples: r.samples,
                }
            })
            .collect();
        tracing::debug!(
            org = %org.0,
            target = %target_id,
            bucket_seconds = bucket,
            buckets = buckets.len(),
            "latency_buckets served from rollup"
        );
        Ok(buckets)
    }

    async fn region_breakdown(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
    ) -> Result<Vec<RegionRollup>> {
        #[derive(Row, Deserialize)]
        struct R {
            region: String,
            samples: u64,
            up: u64,
            quantiles: Vec<f64>,
            last_status: i8,
        }
        let q = self
            .client
            .query(&format!(
                "SELECT region, \
                   countMerge(total_checks) AS samples, \
                   countIfMerge(up_checks) AS up, \
                   quantilesMerge(0.5, 0.95)(duration_quantiles) AS quantiles, \
                   argMaxMerge(last_status_state) AS last_status \
                 FROM check_results_1m \
                 WHERE org_id = ? AND target_id = ? AND {MINUTE_WINDOW} \
                 GROUP BY region ORDER BY region"
            ))
            .bind(org.0)
            .bind(target_id);
        let rows: Vec<R> = bind_minute_window(q, range.from, range.to)
            .fetch_all::<R>()
            .await
            .context("clickhouse region_breakdown")?;
        let ms = |v: f64| v.round().clamp(0.0, u32::MAX as f64) as u32;
        Ok(rows
            .into_iter()
            .map(|r| RegionRollup {
                region: r.region,
                samples: r.samples,
                up: r.up,
                p50_ms: ms(r.quantiles.first().copied().unwrap_or(0.0)),
                p95_ms: ms(r.quantiles.get(1).copied().unwrap_or(0.0)),
                last_status: CheckStatus::from_enum8(r.last_status).as_str().to_string(),
            })
            .collect())
    }

    async fn latency_buckets_by_region(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
    ) -> Result<Vec<RegionLatencySeries>> {
        #[derive(Row, Deserialize)]
        struct LatRow {
            region: String,
            bucket_ts: u32,
            quantiles: Vec<f64>,
            avg: f64,
            dns: f64,
            connect: f64,
            tls: f64,
            ttfb: f64,
            samples: u64,
        }
        let bucket = bucket_seconds.max(60).div_ceil(60) * 60;
        let (table, tcol) = rollup_source(range.from);
        let window = format!("{tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?)");
        let query = format!(
            "SELECT region, \
               toUInt32(toStartOfInterval({tcol}, INTERVAL {bucket} SECOND)) AS bucket_ts, \
               quantilesMerge(0.5, 0.95, 0.99)(duration_quantiles) AS quantiles, \
               avgMerge(avg_duration_ms) AS avg, \
               ifNull(avgMerge(avg_dns_ms), 0) AS dns, \
               ifNull(avgMerge(avg_connect_ms), 0) AS connect, \
               ifNull(avgMerge(avg_tls_ms), 0) AS tls, \
               ifNull(avgMerge(avg_ttfb_ms), 0) AS ttfb, \
               countMerge(total_checks) AS samples \
             FROM {table} \
             WHERE org_id = ? AND target_id = ? AND {window} \
             GROUP BY region, bucket_ts \
             ORDER BY region, bucket_ts"
        );
        let q = self.client.query(&query).bind(org.0).bind(target_id);
        let rows: Vec<LatRow> = bind_minute_window(q, range.from, range.to)
            .fetch_all::<LatRow>()
            .await
            .context("clickhouse latency_buckets_by_region")?;
        let ms = |v: f64| v.round().max(0.0) as u32;
        let mut out: Vec<RegionLatencySeries> = Vec::new();
        for r in rows {
            let q = |i: usize| r.quantiles.get(i).copied().map(ms).unwrap_or(0);
            let bucket = LatencyBucket {
                t: i64::from(r.bucket_ts) * 1000,
                p50: q(0),
                p95: q(1),
                p99: q(2),
                avg: ms(r.avg),
                dns: ms(r.dns),
                connect: ms(r.connect),
                tls: ms(r.tls),
                ttfb: ms(r.ttfb),
                samples: r.samples,
            };
            match out.last_mut() {
                Some(s) if s.region == r.region => s.buckets.push(bucket),
                _ => out.push(RegionLatencySeries {
                    region: r.region,
                    label: String::new(),
                    buckets: vec![bucket],
                }),
            }
        }
        Ok(out)
    }

    async fn fleet_ribbon(
        &self,
        org: OrgId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket_seconds: u32,
        region: Option<&str>,
    ) -> Result<Vec<FleetRibbonBucket>> {
        #[derive(Row, Deserialize)]
        struct RibbonRow {
            bucket_ts: u32,
            samples: u64,
            up: u64,
        }
        // Source matview is per-minute; round up to a multiple of 60s
        // so every output bucket spans an integer number of source rows.
        let bucket = bucket_seconds.max(60).div_ceil(60) * 60;
        // INTERVAL is a SQL keyword position — clickhouse-rs `bind()`
        // produces a bare numeric literal, which is fine, but inlining
        // here keeps `bind()` strictly to value-position arguments.
        let query = format!(
            "SELECT \
               toUInt32(toStartOfInterval(minute, INTERVAL {bucket} SECOND)) AS bucket_ts, \
               countMerge(total_checks) AS samples, \
               countIfMerge(up_checks) AS up \
             FROM check_results_1m \
             WHERE org_id = ? {region_pred} AND {MINUTE_WINDOW} \
             GROUP BY bucket_ts \
             ORDER BY bucket_ts",
            region_pred = region.map(|_| "AND region = ?").unwrap_or("")
        );
        let mut q = self.client.query(&query).bind(org.0);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<RibbonRow> = bind_minute_window(q, from, to)
            .fetch_all::<RibbonRow>()
            .await
            .context("clickhouse fleet_ribbon")?;
        Ok(rows
            .into_iter()
            .map(|r| FleetRibbonBucket {
                bucket_ts: r.bucket_ts as i64,
                samples: r.samples,
                up: r.up,
            })
            .collect())
    }

    async fn prior_period_summary(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<PriorPeriodSummary> {
        // Reads raw `check_results` (not the matview) so the totals come
        // from the same source as `last_n_summary` — the dashboard
        // compares current (from `last_n_summary`) against prior here,
        // and a source mismatch would produce phantom deltas during
        // ingest lag.
        #[derive(Row, Deserialize)]
        struct PriorRow {
            samples: u64,
            up: u64,
            avg_ms: f64,
        }
        let span_secs = (range.to - range.from).num_seconds().max(0);
        let prior_to_secs = range.from.timestamp();
        let prior_from_secs = prior_to_secs.saturating_sub(span_secs);
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT \
                   count() AS samples, \
                   countIf(status = 'up') AS up, \
                   avg(duration_ms) AS avg_ms \
                 FROM {TABLE} \
                 WHERE org_id = ? {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?)"
            ))
            .bind(org.0);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let row: Option<PriorRow> = q
            .bind(prior_from_secs)
            .bind(prior_to_secs)
            .fetch_optional::<PriorRow>()
            .await
            .context("clickhouse prior_period_summary")?;
        let r = row.unwrap_or(PriorRow {
            samples: 0,
            up: 0,
            avg_ms: 0.0,
        });
        let avg_ms = if r.avg_ms.is_finite() {
            r.avg_ms.round().clamp(0.0, u32::MAX as f64) as u32
        } else {
            0
        };
        Ok(PriorPeriodSummary {
            checks_total: r.samples,
            checks_up: r.up,
            avg_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckStatus, MIGRATIONS, split_statements};

    /// Parse the single `Enum8('name' = N, ...)` definition out of the embedded
    /// migration into (name, value) pairs.
    fn check_results_enum8() -> Vec<(String, i8)> {
        let sql = MIGRATIONS[0].1;
        let open = sql
            .find("Enum8(")
            .expect("check_results has an Enum8 column")
            + "Enum8(".len();
        let close = open + sql[open..].find(')').expect("Enum8 close paren");
        sql[open..close]
            .split(',')
            .map(|kv| {
                let (name, val) = kv.split_once('=').expect("Enum8 entry is `name = value`");
                (
                    name.trim().trim_matches('\'').to_string(),
                    val.trim().parse::<i8>().expect("Enum8 value is an int"),
                )
            })
            .collect()
    }

    /// Cross-store contract: every `CheckStatus` must exist in the
    /// `check_results` Enum8 with a matching name+value. ClickHouse `Enum8` is a
    /// closed domain — inserting an undefined key rejects the whole block, so a
    /// new variant without a migration silently dark-holes all ingest.
    #[test]
    fn check_status_matches_clickhouse_enum8() {
        // Exhaustive on purpose: a new `CheckStatus` variant fails to compile
        // here, forcing this list AND the migration Enum8 to be updated together.
        const ALL: &[CheckStatus] = &[
            CheckStatus::Up,
            CheckStatus::Down,
            CheckStatus::Degraded,
            CheckStatus::Error,
        ];
        // Uncalled, but its body is still exhaustiveness-checked at compile
        // time — a new variant turns this into a hard E0004, the forcing signal.
        #[allow(dead_code)]
        fn exhaustiveness_guard(s: CheckStatus) {
            match s {
                CheckStatus::Up
                | CheckStatus::Down
                | CheckStatus::Degraded
                | CheckStatus::Error => {}
            }
        }

        let pairs = check_results_enum8();
        assert_eq!(
            pairs.len(),
            ALL.len(),
            "check_results Enum8 has {} keys but CheckStatus has {} variants: {pairs:?}",
            pairs.len(),
            ALL.len()
        );
        for &s in ALL {
            assert!(
                pairs
                    .iter()
                    .any(|(name, val)| name == s.as_str() && *val == s.as_enum8()),
                "CheckStatus::{s:?} ({}={}) is not in the check_results Enum8 {pairs:?} — \
                 adding a variant requires a ClickHouse migration",
                s.as_str(),
                s.as_enum8()
            );
        }
    }

    #[test]
    fn split_strips_line_comments_before_splitting() {
        // A stray `;` inside a `--` comment used to produce an empty-query
        // chunk that ClickHouse rejected with SYNTAX_ERROR code 62. Regression
        // guard: the splitter must drop the comment text first.
        let sql = "-- prelude with a semi; in it\n\
                   CREATE TABLE foo (x UInt8) ENGINE = TinyLog;\n\
                   -- another; with a semi\n\
                   DROP TABLE foo;";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("CREATE TABLE foo"));
        assert!(stmts[1].contains("DROP TABLE foo"));
    }

    #[test]
    fn split_discards_trailing_blank_chunk() {
        let stmts = split_statements("SELECT 1;\n").expect("test input is well-formed");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "SELECT 1");
    }

    #[test]
    fn split_preserves_inline_comment_after_statement() {
        let stmts = split_statements("SELECT 1; -- trailing\nSELECT 2;")
            .expect("test input is well-formed");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn split_keeps_semicolon_inside_single_quoted_string() {
        // A `;` inside a quoted literal would become a chunk boundary
        // under a naive split, leaving two syntax-error halves. The
        // tokenizer must keep this as one statement.
        let sql = "INSERT INTO t VALUES ('a; b', 'c;');";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0], "INSERT INTO t VALUES ('a; b', 'c;')");
    }

    #[test]
    fn split_handles_doubled_quote_escape() {
        // SQL-standard `''` inside a string represents a single quote and
        // must not close the literal; an unintended close would expose a
        // following `;` to the splitter.
        let sql = "SELECT 'it''s; not over'; SELECT 2;";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], "SELECT 'it''s; not over'");
        assert_eq!(stmts[1], "SELECT 2");
    }

    #[test]
    fn split_handles_backslash_escape_inside_string() {
        // ClickHouse accepts `\'` as an escaped quote. The escape must
        // keep the parser inside the string so the trailing `;` is data.
        let sql = "SELECT 'a\\'b;c'; SELECT 2;";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], "SELECT 'a\\'b;c'");
        assert_eq!(stmts[1], "SELECT 2");
    }

    #[test]
    fn split_keeps_semicolon_inside_double_quoted_identifier() {
        let sql = "SELECT \"a;b\" FROM t; SELECT 2;";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("\"a;b\""));
    }

    #[test]
    fn split_keeps_semicolon_inside_backtick_identifier() {
        let sql = "SELECT `a;b` FROM t; SELECT 2;";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("`a;b`"));
    }

    #[test]
    fn split_strips_block_comment_with_semicolon_inside() {
        let sql = "SELECT 1 /* foo; bar */; SELECT 2;";
        let stmts = split_statements(sql).expect("test input is well-formed");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1], "SELECT 2");
    }

    #[test]
    fn split_comment_only_input_produces_nothing() {
        let stmts = split_statements("-- just a comment\n/* and a block */")
            .expect("test input is well-formed");
        assert!(stmts.is_empty());
    }

    #[test]
    fn split_handles_trailing_statement_without_semicolon() {
        let stmts = split_statements("SELECT 1; SELECT 2").expect("test input is well-formed");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn split_errors_on_unterminated_string_literal() {
        let err = split_statements("INSERT INTO t VALUES ('oops")
            .expect_err("must error on unterminated string");
        let msg = err.to_string();
        assert!(
            msg.contains("unterminated") && msg.contains("string"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn split_errors_on_unterminated_block_comment() {
        let err = split_statements("SELECT 1 /* never closes")
            .expect_err("must error on unterminated block comment");
        let msg = err.to_string();
        assert!(
            msg.contains("unterminated") && msg.contains("block comment"),
            "unexpected error: {msg}"
        );
    }
}
