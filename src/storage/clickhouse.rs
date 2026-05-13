use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use chrono::{DateTime, TimeZone, Utc};
use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::types::StatusBreakdown;
use crate::config::ClickhouseConfig;
use crate::domain::{CheckResult, CheckStatus, Incident, coalesce_incidents};
use crate::error::Result;
use crate::storage::traits::{ResultSink, ResultsStore, TimeRange, UptimeStats};

const TABLE: &str = "check_results";

const MIGRATION_SQL: &str = include_str!("../../migrations/clickhouse/001_initial.sql");

pub async fn migrate(client: &Client) -> Result<()> {
    tracing::info!("running clickhouse migrations");
    for stmt in MIGRATION_SQL.split(';') {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        client
            .query(trimmed)
            .execute()
            .await
            .context("clickhouse migration statement")?;
    }
    tracing::info!("clickhouse ready");
    Ok(())
}

pub fn build_client(cfg: &ClickhouseConfig) -> Client {
    let mut client = Client::default()
        .with_url(&cfg.url)
        .with_database(&cfg.database)
        .with_user(&cfg.user);
    if !cfg.password.is_empty() {
        client = client.with_password(&cfg.password);
    }
    client
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
        let rows: Vec<CheckResultRow<'_>> = results.iter().map(CheckResultRow::from).collect();

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

impl<'a> From<&'a CheckResult> for CheckResultRow<'a> {
    fn from(r: &'a CheckResult) -> Self {
        Self {
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

pub struct ClickhouseResultsStore {
    client: Client,
}

impl ClickhouseResultsStore {
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Narrow projection: only the four columns incident coalescing needs.
    /// Avoids paying for `response_code`/`response_size`/timing fields when
    /// the caller only wants to detect bad-status runs.
    async fn fetch_incident_rows(
        &self,
        target_id: Uuid,
        range: TimeRange,
    ) -> Result<Vec<IncidentRow>> {
        let rows: Vec<IncidentRow> = self
            .client
            .query(&format!(
                "SELECT target_id, timestamp, status, error FROM {TABLE} \
                 WHERE target_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 ORDER BY timestamp ASC"
            ))
            .bind(target_id)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
            .fetch_all::<IncidentRow>()
            .await
            .context("clickhouse fetch_incident_rows")?;
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

fn row_to_result(row: OwnedResultRow) -> CheckResult {
    let timestamp: DateTime<Utc> = Utc
        .timestamp_millis_opt(row.timestamp)
        .single()
        .unwrap_or_else(Utc::now);
    CheckResult {
        target_id: row.target_id,
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
                 WHERE target_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 ORDER BY timestamp DESC LIMIT ? OFFSET ?"
            ))
            .bind(target_id)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
            .bind(limit)
            .bind(offset)
            .fetch_all::<OwnedResultRow>()
            .await
            .context("clickhouse list_results")?;
        Ok(rows.into_iter().map(row_to_result).collect())
    }

    async fn count_results(&self, target_id: Uuid, range: TimeRange) -> Result<u64> {
        #[derive(Row, Deserialize)]
        struct CountRow {
            n: u64,
        }
        let row: CountRow = self
            .client
            .query(&format!(
                "SELECT count() AS n FROM {TABLE} \
                 WHERE target_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?)"
            ))
            .bind(target_id)
            .bind(range.from.timestamp_millis())
            .bind(range.to.timestamp_millis())
            .fetch_one::<CountRow>()
            .await
            .context("clickhouse count_results")?;
        Ok(row.n)
    }

    async fn list_incidents(
        &self,
        target_id: Uuid,
        range: TimeRange,
        ongoing_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Incident>> {
        let rows = self.fetch_incident_rows(target_id, range).await?;
        let mut incidents = coalesce_from_incident_rows(target_id, rows);
        if ongoing_only {
            incidents.retain(|i| i.ended_at.is_none());
        }
        incidents.sort_by_key(|i| std::cmp::Reverse(i.started_at));
        Ok(incidents.into_iter().skip(offset).take(limit).collect())
    }

    async fn count_incidents(
        &self,
        target_id: Uuid,
        range: TimeRange,
        ongoing_only: bool,
    ) -> Result<u64> {
        let rows = self.fetch_incident_rows(target_id, range).await?;
        let mut incidents = coalesce_from_incident_rows(target_id, rows);
        if ongoing_only {
            incidents.retain(|i| i.ended_at.is_none());
        }
        Ok(incidents.len() as u64)
    }

    async fn current_status_breakdown(&self, range: TimeRange) -> Result<StatusBreakdown> {
        #[derive(Row, Deserialize)]
        struct Latest {
            status: i8,
        }
        let rows: Vec<Latest> = self
            .client
            .query(&format!(
                "SELECT argMax(status, timestamp) AS status \
                 FROM {TABLE} \
                 WHERE timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 GROUP BY target_id"
            ))
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

    async fn last_n_summary(&self, range: TimeRange) -> Result<(u64, u64, u64)> {
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
                 WHERE timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?)"
            ))
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
                 WHERE timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?) \
                 ORDER BY target_id ASC, timestamp ASC"
            ))
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
                            coalesce_from_incident_rows(t, std::mem::take(&mut group)).len()
                                as u64;
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

    async fn uptime(&self, target_id: Uuid, range: TimeRange) -> Result<UptimeStats> {
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
                 WHERE target_id = ? \
                 AND timestamp >= fromUnixTimestamp64Milli(?) \
                 AND timestamp < fromUnixTimestamp64Milli(?)"
            ))
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
}
