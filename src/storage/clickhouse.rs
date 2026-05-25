use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use chrono::{DateTime, TimeZone, Utc};
use clickhouse::{Client, Row};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::types::{DashboardMetrics, DashboardSparkBucket, StatusBreakdown};
use crate::config::ClickhouseConfig;
use crate::domain::{
    CheckResult, CheckStatus, Incident, OrgId, coalesce_incidents, coalesce_incidents_bad_only,
};
use crate::error::Result;
use crate::storage::traits::{IncidentListQuery, ResultSink, ResultsStore, TimeRange, UptimeStats};

const TABLE: &str = "check_results";

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
const MIGRATIONS: &[(&str, &str)] = &[(
    "001_initial.sql",
    include_str!("../../migrations/clickhouse/001_initial.sql"),
)];

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

    tracing::info!("clickhouse ready");
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
}

impl ClickhouseResultSink {
    pub fn from_client(client: Client) -> Self {
        Self { client }
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
}

#[async_trait]
impl ResultSink for ClickhouseResultSink {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        let rows: Vec<CheckResultRow<'_>> =
            results.iter().map(CheckResultRow::from_result).collect();

        let backoff = ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(100))
            .with_max_interval(Duration::from_secs(5))
            .with_max_elapsed_time(Some(Duration::from_secs(30)))
            .build();

        // Full re-send on each retry is intentional: ClickHouse `insert_deduplicate` /
        // ReplicatedMergeTree dedup handle partial writes server-side. Don't checkpoint
        // mid-batch here — that races against the server's own dedup window.
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

#[derive(Debug, Row, Serialize)]
struct CheckResultRow<'a> {
    #[serde(with = "clickhouse::serde::uuid")]
    org_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    timestamp: i64,
    status: i8,
    duration_ms: u32,
    dns_ms: Option<u16>,
    connect_ms: Option<u16>,
    tls_ms: Option<u16>,
    ttfb_ms: Option<u16>,
    response_code: Option<u16>,
    response_size: Option<u32>,
    error: Option<&'a str>,
}

impl<'a> CheckResultRow<'a> {
    fn from_result(r: &'a CheckResult) -> Self {
        Self {
            org_id: r.org_id,
            target_id: r.target_id,
            timestamp: r.timestamp.timestamp_millis(),
            status: r.status.as_enum8(),
            duration_ms: r.duration_ms,
            dns_ms: r.dns_ms,
            connect_ms: r.connect_ms,
            tls_ms: r.tls_ms,
            ttfb_ms: r.ttfb_ms,
            response_code: r.response_code,
            response_size: r.response_size,
            error: r.error.as_deref(),
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

    /// Pulls only `down`/`error` observations from CH so the wire and
    /// decode cost stays proportional to *actual* incidents instead of
    /// total check volume. On a healthy 1-min monitor over 30d that's
    /// ~99% fewer rows. Pair with [`coalesce_incidents_bad_only`] — the
    /// all-row coalescer needs `up`/`degraded` markers to detect recovery,
    /// which we no longer pull.
    async fn fetch_bad_only_rows(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
    ) -> Result<Vec<IncidentRow>> {
        let down = CheckStatus::Down.as_enum8();
        let error = CheckStatus::Error.as_enum8();
        let rows: Vec<IncidentRow> = self
            .client
            .query(&format!(
                "SELECT target_id, timestamp, status, error FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? \
                 AND status IN (?, ?) \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 ORDER BY timestamp ASC"
            ))
            .bind(org.0)
            .bind(target_id)
            .bind(down)
            .bind(error)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
            .fetch_all::<IncidentRow>()
            .await
            .context("clickhouse fetch_bad_only_rows")?;
        Ok(rows)
    }
}

#[derive(Debug, Row, Deserialize)]
struct IncidentRow {
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    timestamp: i64,
    status: i8,
    error: Option<String>,
}

fn coalesce_from_incident_rows(target_id: Uuid, rows: Vec<IncidentRow>) -> Vec<Incident> {
    coalesce_incidents(
        target_id,
        rows.into_iter().map(|r| {
            let ts = Utc
                .timestamp_millis_opt(r.timestamp)
                .single()
                .unwrap_or_else(Utc::now);
            (ts, CheckStatus::from_enum8(r.status), r.error)
        }),
    )
}

fn coalesce_from_bad_only_rows(
    target_id: Uuid,
    rows: Vec<IncidentRow>,
    range_end: DateTime<Utc>,
    monitor_interval: std::time::Duration,
) -> Vec<Incident> {
    // 2× the configured interval gives the scheduler one missed tick of
    // grace before we declare a recovery happened in the gap. Floored at
    // 120s so a sub-minute interval still tolerates one missed tick.
    let interval_secs = monitor_interval.as_secs().max(60);
    let threshold = chrono::Duration::seconds((interval_secs * 2) as i64);
    coalesce_incidents_bad_only(
        target_id,
        rows.into_iter().map(|r| {
            let ts = Utc
                .timestamp_millis_opt(r.timestamp)
                .single()
                .unwrap_or_else(Utc::now);
            (ts, CheckStatus::from_enum8(r.status), r.error)
        }),
        range_end,
        threshold,
    )
}

#[derive(Debug, Row, Deserialize)]
struct OwnedResultRow {
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    timestamp: i64,
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

fn row_to_result(row: OwnedResultRow, org_id: Uuid) -> CheckResult {
    let timestamp: DateTime<Utc> = Utc
        .timestamp_millis_opt(row.timestamp)
        .single()
        .unwrap_or_else(Utc::now);
    CheckResult {
        target_id: row.target_id,
        org_id,
        timestamp,
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
    async fn list_results(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckResult>> {
        let limit = limit.min(10_000) as u64;
        let offset = offset as u64;
        let rows: Vec<OwnedResultRow> = self
            .client
            .query(&format!(
                "SELECT target_id, timestamp, status, duration_ms, dns_ms, connect_ms, tls_ms, \
                 ttfb_ms, response_code, response_size, error FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 ORDER BY timestamp DESC LIMIT ? OFFSET ?"
            ))
            .bind(org.0)
            .bind(target_id)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
            .bind(limit)
            .bind(offset)
            .fetch_all::<OwnedResultRow>()
            .await
            .context("clickhouse list_results")?;
        Ok(rows.into_iter().map(|r| row_to_result(r, org.0)).collect())
    }

    async fn list_incidents(
        &self,
        org: OrgId,
        target_id: Uuid,
        query: IncidentListQuery,
    ) -> Result<Vec<Incident>> {
        let range_end = query.range.to;
        let rows = self
            .fetch_bad_only_rows(org, target_id, query.range)
            .await?;
        let mut incidents =
            coalesce_from_bad_only_rows(target_id, rows, range_end, query.monitor_interval);
        if query.ongoing_only {
            incidents.retain(|i| i.ended_at.is_none());
        }
        incidents.sort_by_key(|i| std::cmp::Reverse(i.started_at));
        Ok(incidents
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .collect())
    }

    async fn current_status_breakdown(
        &self,
        org: OrgId,
        range: TimeRange,
    ) -> Result<StatusBreakdown> {
        #[derive(Row, Deserialize)]
        struct Latest {
            status: i8,
        }
        let rows: Vec<Latest> = self
            .client
            .query(&format!(
                "SELECT argMax(status, timestamp) AS status \
                 FROM {TABLE} \
                 WHERE org_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 GROUP BY target_id"
            ))
            .bind(org.0)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
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

    async fn last_n_summary(&self, org: OrgId, range: TimeRange) -> Result<(u64, u64, u64)> {
        #[derive(Row, Deserialize)]
        struct Counts {
            total: u64,
            up: u64,
        }
        let counts: Counts = self
            .client
            .query(&format!(
                "SELECT count() AS total, countIf(status = 'up') AS up \
                 FROM {TABLE} \
                 WHERE org_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?)"
            ))
            .bind(org.0)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
            .fetch_one::<Counts>()
            .await
            .context("clickhouse last_n_summary counts")?;

        // Pull the entire range in one query ordered by (target_id, timestamp).
        // Coalesce per-target client-side; avoids one round-trip per target.
        let rows: Vec<IncidentRow> = self
            .client
            .query(&format!(
                "SELECT target_id, timestamp, status, error FROM {TABLE} \
                 WHERE org_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 ORDER BY target_id ASC, timestamp ASC"
            ))
            .bind(org.0)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
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
        Ok((counts.total, counts.up, incidents))
    }

    async fn uptime(&self, org: OrgId, target_id: Uuid, range: TimeRange) -> Result<UptimeStats> {
        #[derive(Row, Deserialize)]
        struct CountsRow {
            up: u64,
            down: u64,
            degraded: u64,
            error: u64,
        }

        let row: CountsRow = self
            .client
            .query(&format!(
                "SELECT \
                   countIf(status = 'up') AS up, \
                   countIf(status = 'down') AS down, \
                   countIf(status = 'degraded') AS degraded, \
                   countIf(status = 'error') AS error \
                 FROM {TABLE} \
                 WHERE org_id = ? AND target_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?)"
            ))
            .bind(org.0)
            .bind(target_id)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
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
        }
        let from_s = u32::try_from(range.from.timestamp().max(0)).unwrap_or(0);
        let to_s = u32::try_from(range.to.timestamp().max(0)).unwrap_or(u32::MAX);
        let rows: Vec<RollupRow> = self
            .client
            .query(
                "SELECT \
                   target_id, \
                   countMerge(total_checks) AS samples, \
                   countIfMerge(up_checks) AS up, \
                   avgMerge(avg_duration_ms) AS avg_ms, \
                   quantilesMerge(0.5, 0.95)(duration_quantiles) AS quantiles, \
                   argMaxMerge(last_status_state) AS last_status \
                 FROM check_results_1m \
                 WHERE org_id = ? \
                 AND minute >= fromUnixTimestamp(?) \
                 AND minute < fromUnixTimestamp(?) \
                 GROUP BY target_id",
            )
            .bind(org.0)
            .bind(from_s)
            .bind(to_s)
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
                }
            })
            .collect())
    }

    async fn dashboard_sparkline(
        &self,
        org: OrgId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<DashboardSparkBucket>> {
        // Reads the per-minute pre-aggregated rollup so the cost stays
        // O(buckets), independent of the raw sample rate. Two type
        // pitfalls drove the explicit casts here:
        //
        // 1. The view's `minute` is `toStartOfMinute(timestamp)` which
        //    has type `DateTime` (UInt32 seconds), NOT `DateTime64`.
        //    Comparing it against `fromUnixTimestamp64Milli(i64)`
        //    (DateTime64) mixes scales and CH rejects the predicate.
        //    `fromUnixTimestamp(UInt32)` matches the column exactly.
        //
        // 2. `avg_duration_ms` is an `avgState` aggregate column, so
        //    selecting it raw returns the opaque binary state. The
        //    `avgMerge` finaliser is mandatory in `SELECT`.
        #[derive(Row, Deserialize)]
        struct SparkRow {
            #[serde(with = "clickhouse::serde::uuid")]
            target_id: Uuid,
            bucket_ts: u32,
            avg_ms: f64,
        }
        let from_s = u32::try_from(from.timestamp().max(0)).unwrap_or(0);
        let to_s = u32::try_from(to.timestamp().max(0)).unwrap_or(u32::MAX);
        let rows: Vec<SparkRow> = self
            .client
            .query(
                "SELECT \
                   target_id, \
                   toUInt32(minute) AS bucket_ts, \
                   avgMerge(avg_duration_ms) AS avg_ms \
                 FROM check_results_1m \
                 WHERE org_id = ? \
                 AND minute >= fromUnixTimestamp(?) \
                 AND minute < fromUnixTimestamp(?) \
                 GROUP BY target_id, minute \
                 ORDER BY target_id, minute",
            )
            .bind(org.0)
            .bind(from_s)
            .bind(to_s)
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
}

#[cfg(test)]
mod tests {
    use super::split_statements;

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
