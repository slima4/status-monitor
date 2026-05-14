//! Background task that materialises public-component incidents into the
//! Postgres `incidents` table.
//!
//! The writer is **purely a follower** of the existing `check_results` stream
//! — it never modifies the hot write path, never gates check execution, and
//! never produces alerts. Its single job is to keep the `incidents` table in
//! sync with what the recent check results say.
//!
//! Detection rule:
//!  * `≥ flap_threshold` consecutive non-`up` results, no open incident →
//!    INSERT a new open incident.
//!  * `≥ flap_threshold` consecutive `up` results while an open incident
//!    exists → UPDATE `ended_at` to the timestamp of the first `up` in that
//!    trailing run.
//!
//! Both rules are idempotent: re-running with the same input produces no
//! additional writes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::{FromRow, PgPool};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, OrgId, Target};
use crate::error::Result;
use crate::storage::{ResultsStore, TargetFilter, TargetStore, TimeRange};

/// Persistence handle for the `incidents` table — abstracted so the writer
/// can be unit-tested without a live database.
#[async_trait]
pub trait IncidentStore: Send + Sync {
    async fn open_for_target(&self, target_id: Uuid) -> Result<Option<OpenIncident>>;
    async fn insert_open(&self, new: NewOpenIncident) -> Result<Uuid>;
    async fn close(&self, incident_id: Uuid, ended_at: DateTime<Utc>) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct OpenIncident {
    pub id: Uuid,
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewOpenIncident {
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub status_at_start: CheckStatus,
    pub check_count: u32,
    pub error_sample: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IncidentWriterConfig {
    /// How often the writer wakes up and scans every public component.
    pub tick_interval: Duration,
    /// How far back the writer looks at recent check results when deciding
    /// transitions.
    pub lookback: ChronoDuration,
    /// Minimum consecutive checks needed to confirm a transition. Default 2
    /// absorbs single-result flaps.
    pub flap_threshold: u32,
    /// Max results fetched per component per tick. A safety cap so a hot
    /// loop of high-frequency checks doesn't blow up memory.
    pub max_results_per_tick: usize,
}

impl Default for IncidentWriterConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
            lookback: ChronoDuration::minutes(10),
            flap_threshold: 2,
            max_results_per_tick: 1_000,
        }
    }
}

pub struct IncidentWriter {
    target_store: Arc<dyn TargetStore>,
    results_store: Arc<dyn ResultsStore>,
    incident_store: Arc<dyn IncidentStore>,
    cfg: IncidentWriterConfig,
}

impl IncidentWriter {
    pub fn new(
        target_store: Arc<dyn TargetStore>,
        results_store: Arc<dyn ResultsStore>,
        incident_store: Arc<dyn IncidentStore>,
        cfg: IncidentWriterConfig,
    ) -> Self {
        Self {
            target_store,
            results_store,
            incident_store,
            cfg,
        }
    }

    /// Drives the writer until `shutdown` fires. Errors during a tick are
    /// logged but never abort the loop — the next tick retries from scratch.
    pub async fn run(&self, shutdown: CancellationToken) {
        let mut ticker = interval(self.cfg.tick_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // First tick fires immediately; useful so unit tests don't have to
        // sleep through the interval to observe one cycle.
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("incident_writer: shutdown received");
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(err) = self.tick_once().await {
                        tracing::warn!(error = %err, "incident_writer tick failed");
                    }
                }
            }
        }
    }

    /// One iteration of the writer over every public component. Visible for
    /// tests so they can drive a deterministic single tick without sleeping.
    pub async fn tick_once(&self) -> Result<()> {
        let public = self.list_public_targets().await?;
        let now = Utc::now();
        let from = now - self.cfg.lookback;
        let range = TimeRange { from, to: now };
        for t in public {
            if let Err(err) = self.process_target(&t, range).await {
                tracing::warn!(target_id = %t.id, error = %err, "incident_writer per-target failed");
            }
        }
        Ok(())
    }

    async fn list_public_targets(&self) -> Result<Vec<Target>> {
        let targets = self
            .target_store
            .list(TargetFilter {
                limit: Some(10_000),
                offset: 0,
                tag: None,
                enabled: None,
            })
            .await?;
        Ok(targets.into_iter().filter(|t| t.public_status).collect())
    }

    async fn process_target(&self, target: &Target, range: TimeRange) -> Result<()> {
        let mut results = self
            .results_store
            .list_results(target.id, range, self.cfg.max_results_per_tick, 0)
            .await?;
        // Storage returns DESC by timestamp; algorithm operates on ASC.
        results.sort_by_key(|r| r.timestamp);

        let open = self.incident_store.open_for_target(target.id).await?;
        match decide(open.as_ref(), &results, self.cfg.flap_threshold) {
            Action::None => {}
            Action::Open(new) => {
                let id = self.incident_store.insert_open(new).await?;
                tracing::info!(target_id = %target.id, incident_id = %id, "incident opened");
            }
            Action::Close {
                incident_id,
                ended_at,
            } => {
                self.incident_store.close(incident_id, ended_at).await?;
                tracing::info!(target_id = %target.id, incident_id = %incident_id, "incident closed");
            }
        }
        Ok(())
    }
}

/// Decision produced by [`decide`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Open(NewOpenIncident),
    Close {
        incident_id: Uuid,
        ended_at: DateTime<Utc>,
    },
}

impl PartialEq for NewOpenIncident {
    fn eq(&self, other: &Self) -> bool {
        self.target_id == other.target_id
            && self.started_at == other.started_at
            && self.status_at_start == other.status_at_start
            && self.check_count == other.check_count
            && self.error_sample == other.error_sample
    }
}

/// Pure decision function: given the current open-incident state and a list
/// of recent results sorted ascending by timestamp, return what should happen.
///
/// **Idempotency**: this function is referentially transparent. Running it
/// twice on the same `(open, results)` returns the same `Action`, and any
/// `Action::Open` it returns assumes the caller has just verified there is
/// no open incident — running again after the write naturally falls through
/// to `Action::None` because the open incident is now present.
pub fn decide(open: Option<&OpenIncident>, results: &[CheckResult], flap_threshold: u32) -> Action {
    if results.is_empty() {
        return Action::None;
    }
    let threshold = flap_threshold as usize;
    let target_id = results[0].target_id;

    match open {
        Some(inc) => {
            let tail_up = trailing_up_run(results);
            if tail_up.len() >= threshold {
                // Only count up-results that happened AFTER the incident
                // started; otherwise we could mistakenly close an incident
                // using stale `up` rows that pre-date its start.
                let recovery_start = &tail_up[0];
                if recovery_start.timestamp > inc.started_at {
                    return Action::Close {
                        incident_id: inc.id,
                        ended_at: recovery_start.timestamp,
                    };
                }
            }
            Action::None
        }
        None => {
            let tail_bad = trailing_bad_run(results);
            if tail_bad.len() >= threshold {
                let first = &tail_bad[0];
                return Action::Open(NewOpenIncident {
                    target_id,
                    started_at: first.timestamp,
                    status_at_start: first.status,
                    check_count: tail_bad.len() as u32,
                    error_sample: tail_bad.iter().find_map(|r| r.error.clone()),
                });
            }
            Action::None
        }
    }
}

fn trailing_bad_run(results: &[CheckResult]) -> &[CheckResult] {
    let split = results
        .iter()
        .rposition(|r| matches!(r.status, CheckStatus::Up))
        .map(|i| i + 1)
        .unwrap_or(0);
    &results[split..]
}

fn trailing_up_run(results: &[CheckResult]) -> &[CheckResult] {
    let split = results
        .iter()
        .rposition(|r| !matches!(r.status, CheckStatus::Up))
        .map(|i| i + 1)
        .unwrap_or(0);
    &results[split..]
}

// ── PostgreSQL implementation ────────────────────────────────────────────────

/// Org-scoped writer. The single-process materialiser today only services the
/// default org (`tenancy.enabled = false`); a SaaS deployment that runs one
/// materialiser across every tenant must either route each `CheckResult` to a
/// per-org writer or replace this with an `AdminRepo`-style cross-org variant
/// that derives `org_id` from the underlying target row.
pub struct PgIncidentStore {
    pool: PgPool,
    default_org_id: OrgId,
}

impl PgIncidentStore {
    pub fn new(pool: PgPool, default_org_id: OrgId) -> Self {
        Self {
            pool,
            default_org_id,
        }
    }
}

#[derive(FromRow)]
struct OpenIncidentRow {
    id: Uuid,
    target_id: Uuid,
    started_at: DateTime<Utc>,
}

#[async_trait]
impl IncidentStore for PgIncidentStore {
    async fn open_for_target(&self, target_id: Uuid) -> Result<Option<OpenIncident>> {
        let row: Option<OpenIncidentRow> = sqlx::query_as::<_, OpenIncidentRow>(
            r#"SELECT id, target_id, started_at FROM incidents
               WHERE target_id = $1 AND org_id = $2 AND ended_at IS NULL
               ORDER BY started_at DESC LIMIT 1"#,
        )
        .bind(target_id)
        .bind(self.default_org_id.0)
        .fetch_optional(&self.pool)
        .await
        .context("incident open_for_target")?;
        Ok(row.map(|r| OpenIncident {
            id: r.id,
            target_id: r.target_id,
            started_at: r.started_at,
        }))
    }

    async fn insert_open(&self, new: NewOpenIncident) -> Result<Uuid> {
        let status_at_start = status_to_db(new.status_at_start)
            .ok_or_else(|| anyhow::anyhow!("cannot open incident from status=up"))?;
        // Defensive: avoid two open incidents per target if a competing
        // writer raced us (single-process today, but cheap).
        let row: (Uuid,) = sqlx::query_as(
            r#"INSERT INTO incidents (org_id, target_id, started_at, status_at_start, check_count, error_sample)
               SELECT $6, $1, $2, $3, $4, $5
               WHERE NOT EXISTS (
                   SELECT 1 FROM incidents
                   WHERE target_id = $1 AND org_id = $6 AND ended_at IS NULL
               )
               RETURNING id"#,
        )
        .bind(new.target_id)
        .bind(new.started_at)
        .bind(status_at_start)
        .bind(new.check_count as i32)
        .bind(new.error_sample)
        .bind(self.default_org_id.0)
        .fetch_one(&self.pool)
        .await
        .context("incident insert_open")?;
        Ok(row.0)
    }

    async fn close(&self, incident_id: Uuid, ended_at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            r#"UPDATE incidents
               SET ended_at = $2,
                   duration_secs = GREATEST(0, EXTRACT(EPOCH FROM ($2 - started_at))::int),
                   updated_at = now()
               WHERE id = $1 AND org_id = $3 AND ended_at IS NULL"#,
        )
        .bind(incident_id)
        .bind(ended_at)
        .bind(self.default_org_id.0)
        .execute(&self.pool)
        .await
        .context("incident close")?;
        Ok(())
    }
}

fn status_to_db(s: CheckStatus) -> Option<&'static str> {
    match s {
        CheckStatus::Down => Some("down"),
        CheckStatus::Degraded => Some("degraded"),
        CheckStatus::Error => Some("error"),
        CheckStatus::Up => None,
    }
}

// ── In-memory implementation (for tests) ────────────────────────────────────

#[derive(Default)]
pub struct InMemoryIncidentStore {
    inner: parking_lot::Mutex<InMemoryIncidentState>,
}

#[derive(Default)]
struct InMemoryIncidentState {
    by_target: std::collections::HashMap<Uuid, Vec<MemIncident>>,
    inserts: u64,
    closes: u64,
}

#[derive(Debug, Clone)]
pub struct MemIncident {
    pub id: Uuid,
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status_at_start: CheckStatus,
    pub check_count: u32,
    pub error_sample: Option<String>,
}

impl InMemoryIncidentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all_for(&self, target_id: Uuid) -> Vec<MemIncident> {
        self.inner
            .lock()
            .by_target
            .get(&target_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn insert_count(&self) -> u64 {
        self.inner.lock().inserts
    }

    pub fn close_count(&self) -> u64 {
        self.inner.lock().closes
    }
}

#[async_trait]
impl IncidentStore for InMemoryIncidentStore {
    async fn open_for_target(&self, target_id: Uuid) -> Result<Option<OpenIncident>> {
        let g = self.inner.lock();
        let Some(rows) = g.by_target.get(&target_id) else {
            return Ok(None);
        };
        let open = rows
            .iter()
            .filter(|i| i.ended_at.is_none())
            .max_by_key(|i| i.started_at)
            .map(|i| OpenIncident {
                id: i.id,
                target_id: i.target_id,
                started_at: i.started_at,
            });
        Ok(open)
    }

    async fn insert_open(&self, new: NewOpenIncident) -> Result<Uuid> {
        let mut g = self.inner.lock();
        let bucket = g.by_target.entry(new.target_id).or_default();
        if bucket.iter().any(|i| i.ended_at.is_none()) {
            // Idempotent guard: don't double-open.
            return Ok(bucket
                .iter()
                .find(|i| i.ended_at.is_none())
                .map(|i| i.id)
                .unwrap_or_else(Uuid::nil));
        }
        let id = Uuid::now_v7();
        bucket.push(MemIncident {
            id,
            target_id: new.target_id,
            started_at: new.started_at,
            ended_at: None,
            status_at_start: new.status_at_start,
            check_count: new.check_count,
            error_sample: new.error_sample,
        });
        g.inserts += 1;
        Ok(id)
    }

    async fn close(&self, incident_id: Uuid, ended_at: DateTime<Utc>) -> Result<()> {
        let mut g = self.inner.lock();
        for bucket in g.by_target.values_mut() {
            for inc in bucket.iter_mut() {
                if inc.id == incident_id && inc.ended_at.is_none() {
                    inc.ended_at = Some(ended_at);
                }
            }
        }
        g.closes += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use chrono::TimeZone;

    use crate::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod, Target, TargetAlerts};
    use crate::storage::{InMemorySink, InMemoryTargetStore, ResultSink, TargetStore};

    use super::*;

    fn ts(base: DateTime<Utc>, secs: i64) -> DateTime<Utc> {
        base + ChronoDuration::seconds(secs)
    }

    fn result(target_id: Uuid, when: DateTime<Utc>, status: CheckStatus) -> CheckResult {
        CheckResult {
            target_id,
            timestamp: when,
            status,
            duration_ms: 1,
            dns_ms: None,
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: None,
        }
    }

    // ── pure decide() ──────────────────────────────────────────────────────

    #[test]
    fn decide_no_results_is_noop() {
        let action = decide(None, &[], 2);
        assert_eq!(action, Action::None);
    }

    #[test]
    fn decide_single_bad_then_recovery_does_not_open() {
        // [bad, up] — single bad swallowed by the 2-check threshold.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Down),
            result(target, ts(base, 30), CheckStatus::Up),
        ];
        assert_eq!(decide(None, &results, 2), Action::None);
    }

    #[test]
    fn decide_two_consecutive_bad_opens_incident() {
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Up),
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Down),
        ];
        match decide(None, &results, 2) {
            Action::Open(new) => {
                assert_eq!(new.target_id, target);
                assert_eq!(new.started_at, ts(base, 30));
                assert_eq!(new.status_at_start, CheckStatus::Down);
                assert_eq!(new.check_count, 2);
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn decide_three_bad_run_carries_count() {
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Error),
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Down),
        ];
        match decide(None, &results, 2) {
            Action::Open(new) => {
                assert_eq!(new.check_count, 3);
                // First non-up sets the kick-off status.
                assert_eq!(new.status_at_start, CheckStatus::Error);
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn decide_two_good_closes_open_incident() {
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            started_at: ts(base, 0),
        };
        let results = vec![
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Up),
            result(target, ts(base, 90), CheckStatus::Up),
        ];
        match decide(Some(&open), &results, 2) {
            Action::Close {
                incident_id,
                ended_at,
            } => {
                assert_eq!(incident_id, open.id);
                assert_eq!(ended_at, ts(base, 60));
            }
            other => panic!("expected Close, got {other:?}"),
        }
    }

    #[test]
    fn decide_single_good_does_not_close_open_incident() {
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            started_at: ts(base, 0),
        };
        let results = vec![
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Up),
        ];
        assert_eq!(decide(Some(&open), &results, 2), Action::None);
    }

    #[test]
    fn decide_recovery_run_before_incident_does_not_close() {
        // Stale up-rows pre-date the incident — shouldn't fool us into closing.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            started_at: ts(base, 1_000),
        };
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Up),
            result(target, ts(base, 30), CheckStatus::Up),
        ];
        // Tail-up exists but pre-dates incident.started_at → no action.
        assert_eq!(decide(Some(&open), &results, 2), Action::None);
    }

    #[test]
    fn decide_running_twice_with_same_data_is_idempotent_for_open() {
        // After an Open, the caller writes it back. Re-running decide() with
        // the same results but now-known open incident produces Action::None.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Down),
            result(target, ts(base, 30), CheckStatus::Down),
        ];
        match decide(None, &results, 2) {
            Action::Open(_) => {}
            other => panic!("expected Open, got {other:?}"),
        }
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            started_at: ts(base, 0),
        };
        // Same input, but now we know about the open incident; trailing 'up'
        // run length is 0, so nothing happens.
        assert_eq!(decide(Some(&open), &results, 2), Action::None);
    }

    // ── full writer tick with InMemoryIncidentStore ─────────────────────────

    fn make_public_target(name: &str) -> Target {
        Target {
            id: Uuid::now_v7(),
            name: name.into(),
            check: CheckSpec::Http(HttpCheck {
                url: url::Url::parse("https://example.com/").unwrap(),
                method: HttpMethod::Get,
                timeout: StdDuration::from_secs(5),
                follow_redirects: false,
                max_redirects: 0,
                expected_status: ExpectedStatus::Exact(200),
                expected_body_contains: None,
                headers: std::collections::HashMap::new(),
                body: None,
                verify_tls: true,
                basic_auth: None,
                bearer_token: None,
            }),
            interval: StdDuration::from_secs(30),
            enabled: true,
            tags: vec![],
            alerts: TargetAlerts::default(),
            public_status: true,
            public_name: None,
            public_description: None,
            public_group: None,
            public_sort_order: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    async fn seed_results(sink: &InMemorySink, results: Vec<CheckResult>) {
        sink.write_batch(&results).await.expect("seed results");
    }

    fn writer(
        targets: Arc<InMemoryTargetStore>,
        sink: Arc<InMemorySink>,
        incidents: Arc<InMemoryIncidentStore>,
    ) -> IncidentWriter {
        let cfg = IncidentWriterConfig {
            tick_interval: StdDuration::from_secs(1),
            lookback: ChronoDuration::days(1),
            flap_threshold: 2,
            max_results_per_tick: 10_000,
        };
        IncidentWriter::new(
            targets as Arc<dyn TargetStore>,
            sink as Arc<dyn crate::storage::ResultsStore>,
            incidents as Arc<dyn IncidentStore>,
            cfg,
        )
    }

    #[tokio::test]
    async fn tick_does_not_open_on_single_bad_then_recovery() {
        let target = make_public_target("api");
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(60),
                    CheckStatus::Down,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(30),
                    CheckStatus::Up,
                ),
            ],
        )
        .await;

        let w = writer(targets, sink, incidents.clone());
        w.tick_once().await.expect("tick");
        assert_eq!(incidents.insert_count(), 0);
        assert!(incidents.all_for(target_id).is_empty());
    }

    #[tokio::test]
    async fn tick_opens_on_two_consecutive_bad() {
        let target = make_public_target("api");
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(60),
                    CheckStatus::Down,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(30),
                    CheckStatus::Down,
                ),
            ],
        )
        .await;

        let w = writer(targets, sink, incidents.clone());
        w.tick_once().await.expect("tick");
        let all = incidents.all_for(target_id);
        assert_eq!(all.len(), 1);
        assert!(all[0].ended_at.is_none());
        assert_eq!(all[0].status_at_start, CheckStatus::Down);
        assert_eq!(all[0].check_count, 2);
    }

    #[tokio::test]
    async fn tick_closes_open_incident_on_two_consecutive_good() {
        // Simulates the realistic sequence: tick sees the bad run and opens
        // an incident; later results arrive showing recovery; next tick
        // observes the trailing up-run and closes.
        let target = make_public_target("api");
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());

        // Step 1: bad run only — tick opens the incident.
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(120),
                    CheckStatus::Down,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(90),
                    CheckStatus::Down,
                ),
            ],
        )
        .await;
        let w = writer(targets, sink.clone(), incidents.clone());
        w.tick_once().await.expect("tick 1 opens");
        assert_eq!(incidents.insert_count(), 1, "first tick must open");
        let opened = incidents.all_for(target_id);
        assert_eq!(opened.len(), 1);
        assert!(opened[0].ended_at.is_none());

        // Step 2: recovery results show up — next tick closes.
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(60),
                    CheckStatus::Up,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(30),
                    CheckStatus::Up,
                ),
            ],
        )
        .await;
        w.tick_once().await.expect("tick 2 closes");
        let all = incidents.all_for(target_id);
        assert_eq!(all.len(), 1);
        let inc = &all[0];
        assert!(inc.ended_at.is_some(), "incident must be closed");
        assert_eq!(inc.ended_at.unwrap(), now - ChronoDuration::seconds(60));
    }

    #[tokio::test]
    async fn re_running_writer_with_no_new_data_is_noop() {
        let target = make_public_target("api");
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(60),
                    CheckStatus::Down,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(30),
                    CheckStatus::Down,
                ),
            ],
        )
        .await;

        let w = writer(targets, sink, incidents.clone());
        for _ in 0..5 {
            w.tick_once().await.expect("tick");
        }
        assert_eq!(incidents.insert_count(), 1, "must not double-insert");
        assert_eq!(incidents.close_count(), 0, "no close without recovery");
    }

    #[tokio::test]
    async fn re_running_after_close_is_noop() {
        let target = make_public_target("api");
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(120),
                    CheckStatus::Down,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(90),
                    CheckStatus::Down,
                ),
            ],
        )
        .await;
        let w = writer(targets, sink.clone(), incidents.clone());
        w.tick_once().await.expect("open");

        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::seconds(60),
                    CheckStatus::Up,
                ),
                result(
                    target_id,
                    now - ChronoDuration::seconds(30),
                    CheckStatus::Up,
                ),
            ],
        )
        .await;
        w.tick_once().await.expect("close");

        let baseline_inserts = incidents.insert_count();
        let baseline_closes = incidents.close_count();
        // Re-running shouldn't churn anything.
        for _ in 0..5 {
            w.tick_once().await.expect("tick");
        }
        assert_eq!(incidents.insert_count(), baseline_inserts);
        assert_eq!(incidents.close_count(), baseline_closes);
    }

    #[tokio::test]
    async fn shutdown_cancels_run_loop() {
        let targets = Arc::new(InMemoryTargetStore::new());
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        let w = writer(targets, sink, incidents);
        let token = CancellationToken::new();
        let handle = {
            let token = token.clone();
            tokio::spawn(async move { w.run(token).await })
        };
        token.cancel();
        tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("run did not exit within deadline")
            .expect("join");
    }
}
