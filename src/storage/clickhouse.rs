use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use chrono::{DateTime, TimeZone, Utc};
use clickhouse::{Client, Row, query::Query};
use metrics::counter;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::types::{
    AvailabilityBucket, DashboardMetrics, DashboardSparkBucket, FleetRibbonBucket, FlowStepBucket,
    FlowStepTrend, LatencyBucket, PriorPeriodSummary, RegionLatencySeries, RegionRollup,
    StatusBreakdown,
};
use crate::config::ClickhouseConfig;
use crate::domain::agent_wire::{ConsoleLine, FlowEvidence, FlowRunRecord, StepOutcome, StepTrace};
use crate::domain::{
    CheckResult, CheckStatus, HeartbeatPingRecord, Incident, OrgId, coalesce_incidents,
};
use crate::error::Result;
use crate::observability::metrics::names;
use crate::quotas::service::RetentionDays;
use crate::storage::org_ttl::OrgTtlDays;
use crate::storage::traits::{
    ClampedRange, FlowRunSink, FlowRunView, HeartbeatPingSink, ResultSink, ResultsStore, TimeRange,
    UptimeStats, rollup_bucket_secs,
};

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
    // 003 `flow_runs`: one row per browser-flow run, carrying the step trace and
    // — on a failure — the page snapshot. Two retention windows in one table via
    // a per-column TTL driven by `evidence_days`, so page content is dropped
    // ahead of the trace beside it with no second table and no mutation job.
    // Org erasure must clear it too (see [`CH_TENANT_TABLES`]).
    (
        "003_flow_runs.sql",
        include_str!("../../migrations/clickhouse/003_flow_runs.sql"),
    ),
    // 004 `heartbeat_pings`: the job's own account of its runs, which
    // `check_results` cannot hold. Job output gets the shorter `evidence_days`
    // window, same split as `flow_runs`. Org erasure must clear it too (see
    // [`CH_TENANT_TABLES`]).
    (
        "004_heartbeat_pings.sql",
        include_str!("../../migrations/clickhouse/004_heartbeat_pings.sql"),
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

/// Retry budget for an insert. Bounded well under the agent's own request
/// timeout so a stuck ClickHouse surfaces as a failed write rather than a
/// hanging caller.
fn insert_backoff() -> backoff::ExponentialBackoff {
    ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_millis(100))
        .with_max_interval(Duration::from_secs(5))
        .with_max_elapsed_time(Some(Duration::from_secs(30)))
        .build()
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
            .map(|(r, ttl)| CheckResultRow::from_result(r, region, agent_id, ttl.row))
            .collect();

        let backoff = insert_backoff();

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

const FLOW_TABLE: &str = "flow_runs";

/// One row per browser-flow run. Unbatched: at the 300s interval floor these
/// arrive too rarely for a batcher to earn its keep.
pub struct ClickhouseFlowRunSink {
    client: Client,
    /// Region stamped on runs from this control plane's own scheduler.
    /// Agent-submitted runs carry their own via [`FlowRunSink::write_runs_tagged`].
    region: String,
    org_ttl: OrgTtlDays,
}

impl ClickhouseFlowRunSink {
    pub fn new(client: Client, region: String, org_ttl: OrgTtlDays) -> Self {
        Self {
            client,
            region,
            org_ttl,
        }
    }

    async fn write_once(
        &self,
        rows: &[FlowRunRow<'_>],
    ) -> std::result::Result<(), clickhouse::error::Error> {
        let mut insert = self.client.insert::<FlowRunRow>(FLOW_TABLE).await?;
        for row in rows {
            insert.write(row).await?;
        }
        insert.end().await
    }
}

#[async_trait]
impl FlowRunSink for ClickhouseFlowRunSink {
    async fn write_runs(&self, runs: &[FlowRunRecord]) {
        self.write_inner(runs, &self.region).await;
    }

    async fn write_runs_tagged(&self, runs: &[FlowRunRecord], region: &str) {
        self.write_inner(runs, region).await;
    }
}

impl ClickhouseFlowRunSink {
    async fn write_inner(&self, runs: &[FlowRunRecord], region: &str) {
        if runs.is_empty() {
            return;
        }
        let ttls = self.org_ttl.days_for_each(runs.iter().map(|r| r.org_id));
        let rows: Vec<FlowRunRow<'_>> = runs
            .iter()
            .zip(ttls)
            .map(|(r, ttl)| FlowRunRow::from_record(r, region, ttl))
            .collect();

        let backoff = insert_backoff();
        let op = || async {
            self.write_once(&rows)
                .await
                .map_err(backoff::Error::transient)
        };
        if let Err(err) = backoff::future::retry(backoff, op).await {
            // The verdict landed by its own path, so this costs history only.
            tracing::error!(
                ?err,
                count = rows.len(),
                "flow run insert failed after retries"
            );
            counter!(names::STORAGE_DROPPED, "reason" => "flow_run_write_failed")
                .increment(rows.len() as u64);
        }
    }
}

#[derive(Debug, Row, Serialize)]
struct FlowRunRow<'a> {
    #[serde(with = "clickhouse::serde::uuid")]
    org_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    region: &'a str,
    timestamp: u32,
    status: i8,
    duration_ms: u32,
    stopped_step: Option<u16>,
    error: &'a str,
    step_op: Vec<&'a str>,
    step_outcome: Vec<i8>,
    step_ms: Vec<u32>,
    final_url: &'a str,
    title: &'a str,
    text_snippet: &'a str,
    console_level: Vec<&'a str>,
    console_text: Vec<&'a str>,
    evidence_days: u16,
    ttl_days: u16,
}

impl<'a> FlowRunRow<'a> {
    fn from_record(r: &'a FlowRunRecord, region: &'a str, ttl: RetentionDays) -> Self {
        let ev = r.evidence.as_ref();
        let text = |o: &'a Option<String>| o.as_deref().unwrap_or_default();
        Self {
            org_id: r.org_id,
            target_id: r.target_id,
            region,
            timestamp: to_unix_secs(r.timestamp),
            status: r.status.as_enum8(),
            duration_ms: r.duration_ms,
            // The first entry that did not pass: the step it failed on, or the
            // one it never got to when the budget ran out. Either way it is the
            // step the error string names.
            stopped_step: r
                .steps
                .iter()
                .position(|s| s.outcome != StepOutcome::Passed)
                .map(|i| i.min(usize::from(u16::MAX)) as u16),
            error: r.error.as_deref().unwrap_or_default(),
            step_op: r.steps.iter().map(|s| s.op.as_str()).collect(),
            step_outcome: r.steps.iter().map(|s| s.outcome.as_enum8()).collect(),
            step_ms: r.steps.iter().map(|s| s.duration_ms).collect(),
            final_url: ev.map(|e| text(&e.final_url)).unwrap_or_default(),
            title: ev.map(|e| text(&e.title)).unwrap_or_default(),
            text_snippet: ev.map(|e| text(&e.text_snippet)).unwrap_or_default(),
            console_level: ev
                .map(|e| e.console.iter().map(|c| c.level.as_str()).collect())
                .unwrap_or_default(),
            console_text: ev
                .map(|e| e.console.iter().map(|c| c.text.as_str()).collect())
                .unwrap_or_default(),
            evidence_days: ttl.evidence,
            ttl_days: ttl.row,
        }
    }
}

const HEARTBEAT_PING_TABLE: &str = "heartbeat_pings";

/// One row per accepted heartbeat signal. Unbatched: a job pings on its own
/// schedule, and the period floor is 60s, so these arrive too rarely for a
/// batcher to earn its keep.
pub struct ClickhouseHeartbeatPingSink {
    client: Client,
    org_ttl: OrgTtlDays,
}

impl ClickhouseHeartbeatPingSink {
    pub fn new(client: Client, org_ttl: OrgTtlDays) -> Self {
        Self { client, org_ttl }
    }
}

#[async_trait]
impl HeartbeatPingSink for ClickhouseHeartbeatPingSink {
    async fn write_ping(&self, ping: &HeartbeatPingRecord) {
        let row = HeartbeatPingRow::from_record(ping, self.org_ttl.days_for(ping.org_id));

        let backoff = insert_backoff();
        let op = || async {
            let mut insert = self
                .client
                .insert::<HeartbeatPingRow>(HEARTBEAT_PING_TABLE)
                .await
                .map_err(backoff::Error::transient)?;
            insert
                .write(&row)
                .await
                .map_err(backoff::Error::transient)?;
            insert.end().await.map_err(backoff::Error::transient)
        };
        if let Err(err) = backoff::future::retry(backoff, op).await {
            // The ping already moved the state the verdict reads, so this costs
            // history only.
            tracing::error!(?err, "heartbeat ping insert failed after retries");
            counter!(names::STORAGE_DROPPED, "reason" => "heartbeat_ping_write_failed")
                .increment(1);
        }
    }
}

#[derive(Debug, Row, Serialize)]
struct HeartbeatPingRow<'a> {
    #[serde(with = "clickhouse::serde::uuid")]
    org_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    received_at: u32,
    signal: i8,
    exit_code: Option<u8>,
    duration_ms: Option<u32>,
    body: &'a str,
    evidence_days: u16,
    ttl_days: u16,
}

impl<'a> HeartbeatPingRow<'a> {
    fn from_record(r: &'a HeartbeatPingRecord, ttl: RetentionDays) -> Self {
        Self {
            org_id: r.org_id,
            target_id: r.target_id,
            received_at: to_unix_secs(r.received_at),
            signal: r.signal.as_enum8(),
            exit_code: r.exit_code,
            duration_ms: r.duration_ms,
            body: &r.body,
            evidence_days: ttl.evidence,
            ttl_days: ttl.row,
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
struct StoredFlowRunRow {
    timestamp: u32,
    region: String,
    status: i8,
    duration_ms: u32,
    stopped_step: Option<u16>,
    error: String,
    step_op: Vec<String>,
    step_outcome: Vec<i8>,
    step_ms: Vec<u32>,
    final_url: String,
    title: String,
    text_snippet: String,
    console_level: Vec<String>,
    console_text: Vec<String>,
    evidence_expired: u8,
}

/// An expired evidence column reads empty, so it maps back to absent rather
/// than to a blank string the page would render as real.
fn some_if_filled(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

impl From<StoredFlowRunRow> for FlowRunView {
    fn from(r: StoredFlowRunRow) -> Self {
        let steps = r
            .step_op
            .into_iter()
            .zip(r.step_outcome)
            .zip(r.step_ms)
            .map(|((op, outcome), duration_ms)| StepTrace {
                op,
                outcome: StepOutcome::from_enum8(outcome),
                duration_ms,
            })
            .collect();
        let console: Vec<ConsoleLine> = r
            .console_level
            .into_iter()
            .zip(r.console_text)
            .map(|(level, text)| ConsoleLine { level, text })
            .collect();
        let final_url = some_if_filled(r.final_url);
        let title = some_if_filled(r.title);
        let text_snippet = some_if_filled(r.text_snippet);
        let has_page =
            final_url.is_some() || title.is_some() || text_snippet.is_some() || !console.is_empty();
        // The column TTL empties the page on a background merge, not when the
        // window closes, so the window decides — never what is still there.
        let past_window = r.evidence_expired != 0;
        Self {
            timestamp: from_unix_secs(r.timestamp),
            region: r.region,
            status: CheckStatus::from_enum8(r.status),
            duration_ms: r.duration_ms,
            stopped_step: r.stopped_step.map(usize::from),
            error: some_if_filled(r.error),
            steps,
            // Only a run that stopped at a step ever captured one to lose.
            evidence_expired: past_window && r.stopped_step.is_some(),
            evidence: (has_page && !past_window).then_some(FlowEvidence {
                final_url,
                title,
                text_snippet,
                console,
            }),
        }
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

#[derive(Debug, Row, Deserialize)]
struct MultiResultRow {
    #[serde(with = "clickhouse::serde::uuid")]
    org_id: Uuid,
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

impl MultiResultRow {
    fn split(self) -> (String, CheckResult) {
        let org_id = self.org_id;
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
    async fn flow_runs(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        region: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FlowRunView>> {
        let limit = limit.min(500) as u64;
        let region_clause = if region.is_some() {
            "AND region = ? "
        } else {
            ""
        };
        // The newest runs answer "is it healthy now"; the newest failures answer
        // "what went wrong", and at the interval floor those are rarely the same
        // rows — a page of newest-only reaches back hours while the table holds
        // weeks. UNION DISTINCT folds a run that is both into one row.
        // The page columns read empty both when they expired and when the run
        // never captured one, so each branch decides which from the window the
        // row itself carries. The outer select reads that answer back by name;
        // `evidence_days` is not projected, so it cannot recompute it.
        let cols = "timestamp, region, status, duration_ms, stopped_step, error, \
             step_op, step_outcome, step_ms, final_url, title, text_snippet, \
             console_level, console_text";
        let branch_cols =
            format!("{cols}, timestamp < now() - toIntervalDay(evidence_days) AS evidence_expired");
        let outer_cols = format!("{cols}, evidence_expired");
        let scope = format!(
            "org_id = ? AND target_id = ? \
             AND timestamp >= fromUnixTimestamp(?) AND timestamp < fromUnixTimestamp(?) \
             {region_clause}"
        );
        let mut q = self.client.query(&format!(
            "SELECT {outer_cols} FROM (\
                 SELECT {branch_cols} FROM {FLOW_TABLE} WHERE {scope} \
                 ORDER BY timestamp DESC LIMIT {limit} \
                 UNION DISTINCT \
                 SELECT {branch_cols} FROM {FLOW_TABLE} WHERE {scope} AND status != 'up' \
                 ORDER BY timestamp DESC LIMIT {limit}\
             ) ORDER BY timestamp DESC"
        ));
        // Bound twice: once per branch of the union, in source order.
        for _ in 0..2 {
            q = q
                .bind(org.0)
                .bind(target_id)
                .bind(range.from.timestamp())
                .bind(range.to.timestamp());
            if let Some(r) = region {
                q = q.bind(r);
            }
        }
        let rows: Vec<StoredFlowRunRow> = q
            .fetch_all()
            .await
            .context("clickhouse flow_runs for target")?;
        Ok(rows.into_iter().map(FlowRunView::from).collect())
    }

    async fn flow_step_buckets(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
        region: Option<&str>,
    ) -> Result<Vec<FlowStepTrend>> {
        #[derive(Row, Deserialize)]
        struct StepRow {
            step: u16,
            op: String,
            bucket_ts: u32,
            /// `NaN` when the bucket holds no passing run — `avgIf` over an
            /// empty match has no mean to report.
            avg_ms: f64,
            samples: u64,
            failed: u64,
        }
        // Not the rollup grain the other bucketed reads snap to — this one has
        // no rollup and no neighbouring chart to line up with. A floor is still
        // needed: a zero-second interval has no start to round to.
        let bucket = bucket_seconds.max(60);
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        // `arrayEnumerate` fans one run out to a row per step, and the index
        // it yields is the step's own index. A bucket survives on failures
        // alone so the reader can tell a step that only ever fails from one
        // the journey never reached.
        let query = format!(
            "SELECT toUInt16(idx - 1) AS step, \
                    argMax(op, ts) AS op, \
                    toUInt32(toStartOfInterval(ts, INTERVAL {bucket} SECOND)) AS bucket_ts, \
                    avgIf(ms, outcome = 'passed') AS avg_ms, \
                    countIf(outcome = 'passed') AS samples, \
                    countIf(outcome = 'failed') AS failed \
             FROM (\
                 SELECT timestamp AS ts, \
                        arrayJoin(arrayEnumerate(step_ms)) AS idx, \
                        step_op[idx] AS op, \
                        step_outcome[idx] AS outcome, \
                        step_ms[idx] AS ms \
                 FROM {FLOW_TABLE} \
                 WHERE org_id = ? AND target_id = ? {region_pred} \
                   AND timestamp >= fromUnixTimestamp(?) AND timestamp < fromUnixTimestamp(?)\
             ) \
             WHERE outcome != 'skipped' \
             GROUP BY step, bucket_ts \
             ORDER BY step, bucket_ts"
        );
        let mut q = self.client.query(&query).bind(org.0).bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<StepRow> = q
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .fetch_all()
            .await
            .context("clickhouse flow_step_buckets")?;

        // Ordered by step then time, so one pass folds without a map.
        let mut trends: Vec<FlowStepTrend> = Vec::new();
        for r in rows {
            let bucket = FlowStepBucket {
                t: i64::from(r.bucket_ts) * 1000,
                avg: (!r.avg_ms.is_nan()).then(|| r.avg_ms.round().max(0.0) as u32),
                samples: r.samples,
                failed: r.failed,
            };
            match trends.last_mut() {
                // Oldest bucket first, and each carries its own newest op, so
                // the label lands on what the step runs today.
                Some(t) if t.step == r.step => {
                    t.op = r.op;
                    t.buckets.push(bucket);
                }
                _ => trends.push(FlowStepTrend {
                    step: r.step,
                    op: r.op,
                    buckets: vec![bucket],
                }),
            }
        }
        Ok(trends)
    }

    async fn heartbeat_failure_output(
        &self,
        org: OrgId,
        target_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<Option<String>> {
        // `received_at` is second-granularity, so the microseconds Postgres
        // keeps have to go before the instants can be compared.
        let body: Vec<String> = self
            .client
            .query(&format!(
                "SELECT body FROM {HEARTBEAT_PING_TABLE} \
                 WHERE org_id = ? AND target_id = ? AND signal = 'fail' \
                   AND received_at = fromUnixTimestamp(?) LIMIT 1"
            ))
            .bind(org.0)
            .bind(target_id)
            .bind(to_unix_secs(at))
            .fetch_all::<String>()
            .await
            .context("clickhouse heartbeat failure output")?;
        // An expired `body` column reads empty, same as a ping that carried
        // none; neither is worth a panel.
        Ok(body.into_iter().next().filter(|b| !b.is_empty()))
    }

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

    async fn list_failures_by_region(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        limit: usize,
        offset: usize,
        region: Option<&str>,
    ) -> Result<Vec<(String, CheckResult)>> {
        let limit = limit.min(10_000) as u64;
        let offset = offset as u64;
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT region, target_id, timestamp, status, duration_ms, dns_ms, connect_ms, \
                 tls_ms, ttfb_ms, response_code, response_size, error FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? AND status != {up} {region_pred} \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?) \
                 ORDER BY timestamp DESC LIMIT ? OFFSET ?",
                up = CheckStatus::Up.as_enum8(),
            ))
            .bind(org.0)
            .bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<RegionResultRow> = q
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .bind(limit)
            .bind(offset)
            .fetch_all::<RegionResultRow>()
            .await
            .context("clickhouse list_failures_by_region")?;
        Ok(rows.into_iter().map(|r| r.split(org.0)).collect())
    }

    async fn recent_results_for_targets(
        &self,
        targets: &[(OrgId, Uuid)],
        range: ClampedRange,
        per_target_limit: usize,
    ) -> Result<Vec<(String, CheckResult)>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let limit = per_target_limit.min(10_000) as u64;
        // Pair IN over typed, unique ids (injection-safe): hits the
        // (org_id, target_id, ...) sort-key prefix so CH prunes by org instead
        // of scanning every tenant's granules.
        let pair_list = targets
            .iter()
            .map(|(o, t)| format!("('{}','{}')", o.0, t))
            .collect::<Vec<_>>()
            .join(",");
        let rows: Vec<MultiResultRow> = self
            .client
            .query(&format!(
                "SELECT org_id, region, target_id, timestamp, status, duration_ms, dns_ms, \
                 connect_ms, tls_ms, ttfb_ms, response_code, response_size, error FROM {TABLE} \
                 WHERE (org_id, target_id) IN ({pair_list}) \
                 AND timestamp >= fromUnixTimestamp(?) \
                 AND timestamp < fromUnixTimestamp(?) \
                 ORDER BY target_id, region, timestamp DESC \
                 LIMIT {limit} BY target_id, region"
            ))
            .bind(range.from.timestamp())
            .bind(range.to.timestamp())
            .fetch_all::<MultiResultRow>()
            .await
            .context("clickhouse recent_results_for_targets")?;
        Ok(rows.into_iter().map(MultiResultRow::split).collect())
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
        let (table, tcol) = rollup_source(range.from);
        let window = format!("{tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?)");
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut cq = self
            .client
            .query(&format!(
                "SELECT countMerge(total_checks) AS total, \
                        countIfMerge(up_checks) AS up, \
                        avgMerge(avg_duration_ms) AS avg_ms \
                 FROM {table} \
                 WHERE org_id = ? {region_pred} AND {window}"
            ))
            .bind(org.0);
        if let Some(r) = region {
            cq = cq.bind(r);
        }
        let counts: Counts = bind_minute_window(cq, range.from, range.to)
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
            total: u64,
            up: u64,
            down: u64,
            degraded: u64,
            error: u64,
        }

        let (table, tcol) = rollup_source(range.from);
        let window = format!("{tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?)");
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT \
                   countMerge(total_checks) AS total, \
                   countIfMerge(up_checks) AS up, \
                   countIfMerge(down_checks) AS down, \
                   countIfMerge(degraded_checks) AS degraded, \
                   countIfMerge(error_checks) AS error \
                 FROM {table} \
                 WHERE org_id = ? AND target_id = ? {region_pred} AND {window}"
            ))
            .bind(org.0)
            .bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let row: CountsRow = bind_minute_window(q, range.from, range.to)
            .fetch_one::<CountsRow>()
            .await
            .context("clickhouse uptime")?;

        let mut stats = UptimeStats {
            total: row.total,
            up: row.up,
            down: row.down,
            degraded: row.degraded,
            error: row.error,
            ..Default::default()
        };
        stats.finish();
        Ok(stats)
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
            checks: u64,
            up: u64,
        }
        let query = format!(
            "SELECT \
               target_id, \
               toUInt32(minute) AS bucket_ts, \
               avgMerge(avg_duration_ms) AS avg_ms, \
               countMerge(total_checks) AS checks, \
               countIfMerge(up_checks) AS up \
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
                checks: r.checks,
                up: r.up,
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
        let bucket = rollup_bucket_secs(bucket_seconds);
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

    async fn availability_buckets(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
        region: Option<&str>,
    ) -> Result<Vec<AvailabilityBucket>> {
        #[derive(Row, Deserialize)]
        struct AvailRow {
            bucket_ts: u32,
            total: u64,
            up: u64,
        }
        let bucket = rollup_bucket_secs(bucket_seconds);
        let (table, tcol) = rollup_source(range.from);
        let window = format!("{tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?)");
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let query = format!(
            "SELECT \
               toUInt32(toStartOfInterval({tcol}, INTERVAL {bucket} SECOND)) AS bucket_ts, \
               countMerge(total_checks) AS total, \
               countIfMerge(up_checks) AS up \
             FROM {table} \
             WHERE org_id = ? AND target_id = ? {region_pred} AND {window} \
             GROUP BY bucket_ts \
             ORDER BY bucket_ts"
        );
        let mut q = self.client.query(&query).bind(org.0).bind(target_id);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let rows: Vec<AvailRow> = bind_minute_window(q, range.from, range.to)
            .fetch_all::<AvailRow>()
            .await
            .context("clickhouse availability_buckets")?;
        Ok(rows
            .into_iter()
            .map(|r| AvailabilityBucket {
                bucket_ts: i64::from(r.bucket_ts),
                total: r.total,
                up: r.up,
            })
            .collect())
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
                   quantilesMerge(0.5, 0.95, 0.99)(duration_quantiles) AS quantiles, \
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
                p99_ms: ms(r.quantiles.get(2).copied().unwrap_or(0.0)),
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
        let bucket = rollup_bucket_secs(bucket_seconds);
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
            // `Array(UUID)`: the crate's UUID adapter is single-value only, so
            // read each element in its RowBinary `(hi, lo)` form and rebuild.
            down_targets: Vec<(u64, u64)>,
        }
        let bucket = rollup_bucket_secs(bucket_seconds);
        // Inner per-target pass so the outer can sum the fleet rate and also
        // collect which monitors dipped. INTERVAL is a keyword position —
        // inlined so `bind()` stays value-only.
        let query = format!(
            "SELECT bucket_ts, sum(t_samples) AS samples, sum(t_up) AS up, \
                    groupUniqArrayIf(target_id, t_samples > t_up) AS down_targets \
             FROM ( \
               SELECT \
                 toUInt32(toStartOfInterval(minute, INTERVAL {bucket} SECOND)) AS bucket_ts, \
                 target_id, \
                 countMerge(total_checks) AS t_samples, \
                 countIfMerge(up_checks) AS t_up \
               FROM check_results_1m \
               WHERE org_id = ? {region_pred} AND {MINUTE_WINDOW} \
               GROUP BY bucket_ts, target_id \
             ) \
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
                down_targets: r
                    .down_targets
                    .into_iter()
                    .map(|(hi, lo)| Uuid::from_u64_pair(hi, lo))
                    .collect(),
            })
            .collect())
    }

    async fn prior_period_summary(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<PriorPeriodSummary> {
        // Same rollup source as `last_n_summary`: the dashboard compares
        // current (from `last_n_summary`) against prior here, so a source
        // mismatch would produce phantom deltas during ingest lag.
        #[derive(Row, Deserialize)]
        struct PriorRow {
            samples: u64,
            up: u64,
            avg_ms: f64,
        }
        let span = range.to - range.from;
        let prior_to = range.from;
        let prior_from = prior_to - span;
        let (table, tcol) = rollup_source(prior_from);
        let window = format!("{tcol} >= fromUnixTimestamp(?) AND {tcol} < fromUnixTimestamp(?)");
        let region_pred = region.map(|_| "AND region = ?").unwrap_or("");
        let mut q = self
            .client
            .query(&format!(
                "SELECT \
                   countMerge(total_checks) AS samples, \
                   countIfMerge(up_checks) AS up, \
                   avgMerge(avg_duration_ms) AS avg_ms \
                 FROM {table} \
                 WHERE org_id = ? {region_pred} AND {window}"
            ))
            .bind(org.0);
        if let Some(r) = region {
            q = q.bind(r);
        }
        let row: Option<PriorRow> = bind_minute_window(q, prior_from, prior_to)
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

    /// A page the background merge has not yet emptied, past its window.
    fn stored_run(evidence_expired: u8, stopped_step: Option<u16>) -> super::StoredFlowRunRow {
        super::StoredFlowRunRow {
            timestamp: 1_700_000_000,
            region: "eu-helsinki".into(),
            status: 2,
            duration_ms: 3_100,
            stopped_step,
            error: "step 2/2 assert_url: url does not contain \"/secure\"".into(),
            step_op: vec!["fill".into(), "assert_url".into()],
            step_outcome: vec![1, 2],
            step_ms: vec![40, 10_000],
            final_url: "https://app.example.com/login".into(),
            title: "Sign in".into(),
            text_snippet: "Your password is invalid!".into(),
            console_level: Vec::new(),
            console_text: Vec::new(),
            evidence_expired,
        }
    }

    #[test]
    fn a_page_past_its_window_is_dropped_even_before_the_merge_empties_it() {
        let view = super::FlowRunView::from(stored_run(1, Some(1)));
        assert!(view.evidence.is_none());
        assert!(view.evidence_expired);
        assert_eq!(view.steps.len(), 2, "the trace outlives the page");
    }

    #[test]
    fn a_page_inside_its_window_is_returned() {
        let view = super::FlowRunView::from(stored_run(0, Some(1)));
        let evidence = view.evidence.expect("still inside the window");
        assert_eq!(
            evidence.text_snippet.as_deref(),
            Some("Your password is invalid!")
        );
        assert!(!view.evidence_expired);
    }

    #[test]
    fn a_run_that_never_reached_a_step_lost_no_page() {
        assert!(!super::FlowRunView::from(stored_run(1, None)).evidence_expired);
    }
}
