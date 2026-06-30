//! Background task that materialises incidents for every monitor into the
//! Postgres `incidents` table. An incident opens for any enabled target that
//! crosses the failure threshold; whether it is publicly visible is derived at
//! insert time from the monitor's status-page membership.
//!
//! The writer is **purely a follower** of the existing `check_results` stream
//! — it never modifies the hot write path, never gates check execution, and
//! never produces alerts. Its single job is to keep the `incidents` table in
//! sync with what the recent check results say.
//!
//! Detection rule (anything not `up` is unhealthy):
//!  * `≥ flap_threshold` consecutive `down`/`error`/`degraded` results, no open
//!    incident → INSERT a new open incident.
//!  * `≥ flap_threshold` consecutive `up` results while an open incident exists
//!    → UPDATE `ended_at` to the first such timestamp.
//!
//! Both rules are idempotent: re-running with the same input produces no
//! additional writes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::stream::{self, StreamExt};
use sqlx::{FromRow, PgPool};
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, NotificationReason, OrgId, Target};
use crate::error::Result;
use crate::escalation::IncidentSignal;
use crate::storage::ResultsStore;
use crate::storage::admin::{EnabledTargetStream, PublicTargetCursor};
use crate::storage::traits::{ClampedRange, TimeRange};

/// Persistence handle for the `incidents` table — abstracted so the writer
/// can be unit-tested without a live database. Every method takes `org` so a
/// single store instance can service every tenant.
#[async_trait]
pub trait IncidentStore: Send + Sync {
    async fn open_for_target(&self, org: OrgId, target_id: Uuid) -> Result<Option<OpenIncident>>;
    /// Batched cross-tenant lookup: one SQL round-trip resolves the open
    /// `OpenIncident` for every `(org, target)` pair in the page. The list is
    /// 0-or-1 under the unique open-incident index; it stays a `Vec` so a future
    /// region-scoped grain needs no signature change.
    async fn open_for_pairs(
        &self,
        pairs: &[(OrgId, Uuid)],
    ) -> Result<std::collections::HashMap<(OrgId, Uuid), Vec<OpenIncident>>>;
    /// `None` = a concurrent writer already holds the open incident for this
    /// target (the DB unique index won the race); the caller must not page.
    async fn insert_open(&self, org: OrgId, new: NewOpenIncident) -> Result<Option<Uuid>>;
    /// `true` = this call flipped the incident to resolved; `false` = it was
    /// already closed (lost the race), so the caller must not page.
    async fn close(&self, org: OrgId, incident_id: Uuid, ended_at: DateTime<Utc>) -> Result<bool>;
}

#[derive(Debug, Clone)]
pub struct OpenIncident {
    pub id: Uuid,
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
    /// `None` = whole-target incident; `Some(r)` = scoped to one region.
    pub region: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewOpenIncident {
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub status_at_start: CheckStatus,
    pub check_count: u32,
    pub error_sample: Option<String>,
    pub region: Option<String>,
    /// Regions down / still up at open time (empty for a single-region monitor).
    pub regions_down: Vec<String>,
    pub regions_up: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IncidentWriterConfig {
    /// Scan cadence. Must stay below `lookback` or consecutive scans leave a
    /// blind gap; set at the fastest check interval so detection isn't tick-bound.
    pub tick_interval: Duration,
    /// Floor for the per-target lookback window (it grows with each target's
    /// interval; see `lookback_for`) so fast monitors still carry enough history.
    pub lookback: ChronoDuration,
    /// Minimum consecutive checks needed to confirm a transition. Default 2
    /// absorbs single-result flaps.
    pub flap_threshold: u32,
    /// Max results fetched per component per tick. A safety cap so a hot
    /// loop of high-frequency checks doesn't blow up memory.
    pub max_results_per_tick: usize,
    /// Number of `(org, target)` rows loaded per database round-trip during
    /// the cross-tenant walk. Bounds peak writer-side RAM to roughly
    /// `page_size * sizeof(Target)`; independent of total target count.
    pub page_size: usize,
    /// Max concurrent `process_target` futures per page. Keeps peak DB
    /// connection demand bounded so the writer can't starve foreground
    /// traffic. Tune below the Postgres pool size.
    pub max_concurrency: usize,
}

impl Default for IncidentWriterConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
            lookback: ChronoDuration::minutes(10),
            flap_threshold: 2,
            max_results_per_tick: 1_000,
            page_size: 256,
            max_concurrency: 4,
        }
    }
}

pub struct IncidentWriter {
    /// Cross-org stream of every enabled target. The writer never holds a
    /// single org — [`EnabledTargetStream`] is the only surface it talks to for
    /// discovery, and it is keyset-paginated so the per-tick memory and
    /// database load stay bounded as the tenant count grows.
    targets: Arc<dyn EnabledTargetStream>,
    results_store: Arc<dyn ResultsStore>,
    incident_store: Arc<dyn IncidentStore>,
    cfg: IncidentWriterConfig,
    /// Notifies the escalation engine when an incident opens or auto-resolves.
    /// `None` leaves the writer paging-agnostic (tests, escalation disabled).
    signal_tx: Option<mpsc::Sender<IncidentSignal>>,
}

impl IncidentWriter {
    pub fn new(
        targets: Arc<dyn EnabledTargetStream>,
        results_store: Arc<dyn ResultsStore>,
        incident_store: Arc<dyn IncidentStore>,
        cfg: IncidentWriterConfig,
    ) -> Self {
        debug_assert!(
            cfg.lookback
                > ChronoDuration::from_std(cfg.tick_interval).unwrap_or(ChronoDuration::MAX),
            "lookback must exceed tick_interval or scans leave a blind gap"
        );
        Self {
            targets,
            results_store,
            incident_store,
            cfg,
            signal_tx: None,
        }
    }

    /// Wire the escalation-engine signal channel so opens/resolves page.
    pub fn with_signals(mut self, tx: mpsc::Sender<IncidentSignal>) -> Self {
        self.signal_tx = Some(tx);
        self
    }

    fn signal(&self, org: OrgId, incident_id: Uuid, reason: NotificationReason) {
        if let Some(tx) = &self.signal_tx {
            // Non-blocking: incident detection must never stall behind paging
            // throughput. A full channel under a mass outage drops the nudge
            // (logged); the row is still in the DB for a later reconcile.
            if let Err(err) = tx.try_send(IncidentSignal {
                org,
                incident_id,
                reason,
            }) {
                metrics::counter!(
                    crate::observability::metrics::names::ALERTS_DROPPED,
                    "reason" => reason.as_db_str()
                )
                .increment(1);
                tracing::warn!(%org, %incident_id, error = %err, "incident paging signal dropped");
            }
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

    /// One iteration over every enabled target in every live org. Visible
    /// for tests so they can drive a deterministic single tick without
    /// sleeping.
    ///
    /// Walks the cross-tenant target set with keyset pagination so peak RAM
    /// is `O(page_size)` regardless of org count. Per page the open incidents
    /// and recent results are both resolved in batched reads (one round-trip
    /// for opens, one per lookback tier for results, not one per target), then
    /// per-target work runs concurrently up to `max_concurrency`. A per-target
    /// error logs and continues so one tenant's failure can't stall the rest.
    pub async fn tick_once(&self) -> Result<()> {
        let now = Utc::now();
        let concurrency = self.cfg.max_concurrency.max(1);

        let mut cursor: Option<PublicTargetCursor> = None;
        loop {
            let page = self
                .targets
                .next_enabled_target_page(cursor, self.cfg.page_size)
                .await?;
            let Some(last) = page.last() else {
                return Ok(());
            };
            cursor = Some(PublicTargetCursor::after(last.0, last.1.id));

            let pairs: Vec<(OrgId, Uuid)> = page.iter().map(|(o, t)| (*o, t.id)).collect();
            let open_map = Arc::new(self.incident_store.open_for_pairs(&pairs).await?);
            let results = Arc::new(self.load_page_results(&page, now).await?);

            stream::iter(page.into_iter().map(|(org, target)| {
                let open_map = open_map.clone();
                let results = results.clone();
                async move {
                    let open = open_map.get(&(org, target.id)).cloned().unwrap_or_default();
                    let tagged = results.get(&target.id).cloned().unwrap_or_default();
                    if let Err(err) = self.process_target(org, &target, open, tagged, now).await {
                        tracing::warn!(
                            %org,
                            target_id = %target.id,
                            error = %err,
                            "incident_writer per-target failed"
                        );
                    }
                }
            }))
            .buffer_unordered(concurrency)
            .for_each(|_| async {})
            .await;
        }
    }

    /// One ClickHouse read per lookback tier instead of one per target, so a
    /// slow monitor never widens a fast one's scan window. Each target is then
    /// trimmed to its own window in [`process_target`](Self::process_target).
    async fn load_page_results(
        &self,
        page: &[(OrgId, Target)],
        now: DateTime<Utc>,
    ) -> Result<std::collections::HashMap<Uuid, Vec<(String, CheckResult)>>> {
        let mut tiers: std::collections::BTreeMap<i64, Vec<(OrgId, Uuid)>> =
            std::collections::BTreeMap::new();
        for (org, target) in page {
            let tier = tier_seconds(self.lookback_for(target));
            tiers.entry(tier).or_default().push((*org, target.id));
        }

        let mut out: std::collections::HashMap<Uuid, Vec<(String, CheckResult)>> =
            std::collections::HashMap::new();
        for (tier_secs, targets) in tiers {
            let range = ClampedRange::unclamped(TimeRange {
                from: now - ChronoDuration::seconds(tier_secs),
                to: now,
            });
            let rows = self
                .results_store
                .recent_results_for_targets(&targets, range, self.cfg.max_results_per_tick)
                .await?;
            for (region, r) in rows {
                out.entry(r.target_id).or_default().push((region, r));
            }
        }
        Ok(out)
    }

    /// Window sized to the target's cadence: confirming a transition needs
    /// `confirmations` results spaced one `interval` apart, which a fixed window
    /// can't hold for a slow monitor. `cfg.lookback` floors it for fast ones.
    fn lookback_for(&self, target: &Target) -> ChronoDuration {
        let confirmations = u64::from(target.alert_confirmations.max(1));
        let needed =
            ChronoDuration::seconds((target.interval.as_secs() * 2 * confirmations) as i64);
        self.cfg.lookback.max(needed)
    }

    async fn process_target(
        &self,
        org: OrgId,
        target: &Target,
        open: Vec<OpenIncident>,
        tagged: Vec<(String, CheckResult)>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        // The tier read can be wider than this target's window; trim to its own.
        let cutoff = now - self.lookback_for(target);
        // Each region evaluated on its own ASC run, never interleaved.
        let mut by_region: std::collections::BTreeMap<String, Vec<CheckResult>> =
            std::collections::BTreeMap::new();
        for (region, r) in tagged {
            if r.timestamp >= cutoff {
                by_region.entry(region).or_default().push(r);
            }
        }
        let mut by_region: Vec<(String, Vec<CheckResult>)> = by_region.into_iter().collect();
        for (_, results) in by_region.iter_mut() {
            results.sort_by_key(|r| r.timestamp);
        }

        let confirmations = target.alert_confirmations.max(1);
        let quorum = target.region_policy.required(by_region.len());
        let actions = decide_multi(target.id, &open, &by_region, confirmations, quorum);
        for action in actions {
            match action {
                Action::None => {}
                Action::Open(new) => {
                    if let Some(id) = self.incident_store.insert_open(org, new).await? {
                        tracing::info!(%org, target_id = %target.id, incident_id = %id, "incident opened");
                        self.signal(org, id, NotificationReason::Opened);
                    }
                }
                Action::Close {
                    incident_id,
                    ended_at,
                } => {
                    if self
                        .incident_store
                        .close(org, incident_id, ended_at)
                        .await?
                    {
                        tracing::info!(%org, target_id = %target.id, incident_id = %incident_id, "incident closed");
                        self.signal(org, incident_id, NotificationReason::Resolved);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Round a per-target lookback window up to a coarse tier so similar-cadence
/// targets share one batched read. Past the ladder the exact window is used
/// (rare, very slow monitors).
fn tier_seconds(window: ChronoDuration) -> i64 {
    const LADDER: [i64; 6] = [
        15 * 60,
        60 * 60,
        6 * 60 * 60,
        24 * 60 * 60,
        7 * 24 * 60 * 60,
        30 * 24 * 60 * 60,
    ];
    let secs = window.num_seconds().max(1);
    LADDER.into_iter().find(|&t| secs <= t).unwrap_or(secs)
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
            && self.region == other.region
            && self.regions_down == other.regions_down
            && self.regions_up == other.regions_up
    }
}

/// Single-region convenience over [`decide_multi`]: one region, combined
/// any-down policy. Kept for call sites and tests that reason about a flat
/// result stream. Returns at most one [`Action`].
///
/// **Idempotency**: referentially transparent. Any `Action::Open` it returns
/// assumes the caller has just verified there is no open incident — re-running
/// after the write falls through to `Action::None`.
pub fn decide(open: Option<&OpenIncident>, results: &[CheckResult], flap_threshold: u32) -> Action {
    let Some(target_id) = results.first().map(|r| r.target_id) else {
        return Action::None;
    };
    let opens: Vec<OpenIncident> = open.cloned().into_iter().collect();
    let by_region = [(String::new(), results.to_vec())];
    decide_multi(target_id, &opens, &by_region, flap_threshold, 1)
        .into_iter()
        .next()
        .unwrap_or(Action::None)
}

/// Pure region-aware decision. Each `(region, results)` group is one region's
/// checks ascending by time; `opens` is every open incident for the target.
/// `confirmations` is the per-region consecutive-bad run needed; `quorum` is how
/// many regions must agree before the combined incident opens (clamped to the
/// live region count so it can never be unreachable). Returns the writes to
/// apply; an empty vec means nothing to do.
pub fn decide_multi(
    target_id: Uuid,
    opens: &[OpenIncident],
    by_region: &[(String, Vec<CheckResult>)],
    confirmations: u32,
    quorum: usize,
) -> Vec<Action> {
    let threshold = (confirmations as usize).max(1);

    struct Verdict<'a> {
        region: &'a str,
        bad: &'a [CheckResult],
        good: &'a [CheckResult],
    }
    let verdicts: Vec<Verdict> = by_region
        .iter()
        .map(|(region, results)| Verdict {
            region,
            bad: trailing_bad_run(results),
            good: trailing_good_run(results),
        })
        .collect();

    let quorum = quorum.clamp(1, verdicts.len().max(1));
    let mut bad: Vec<&Verdict> = verdicts
        .iter()
        .filter(|v| v.bad.len() >= threshold)
        .collect();
    bad.sort_by_key(|v| v.bad[0].timestamp);
    let combined = opens.iter().find(|i| i.region.is_none());

    match combined {
        None => {
            if bad.len() >= quorum {
                // region = None: one whole-target incident, so its key must be
                // region-independent or the next tick re-opens it.
                let trigger = bad[quorum - 1];
                let origin = bad[0];
                vec![Action::Open(NewOpenIncident {
                    target_id,
                    started_at: trigger.bad[0].timestamp,
                    status_at_start: origin.bad[0].status,
                    check_count: origin.bad.len() as u32,
                    error_sample: bad
                        .iter()
                        .find_map(|v| v.bad.iter().find_map(|r| r.error.clone())),
                    region: None,
                    regions_down: bad
                        .iter()
                        .map(|v| v.region)
                        .filter(|r| !r.is_empty())
                        .map(str::to_string)
                        .collect(),
                    regions_up: verdicts
                        .iter()
                        .filter(|v| v.bad.len() < threshold && !v.region.is_empty())
                        .map(|v| v.region.to_string())
                        .collect(),
                })]
            } else {
                vec![]
            }
        }
        Some(inc) => {
            // Close once below quorum, with a sustained-good region as recovery
            // evidence; ended_at = latest such recovery.
            if bad.len() < quorum {
                let ended = verdicts
                    .iter()
                    .filter(|v| v.good.len() >= threshold)
                    .map(|v| v.good[0].timestamp)
                    .max();
                if let Some(ended) = ended
                    && ended > inc.started_at
                {
                    return vec![Action::Close {
                        incident_id: inc.id,
                        ended_at: ended,
                    }];
                }
            }
            vec![]
        }
    }
}

/// Anything that is not a clean `Up` is unhealthy: `Down`/`Error` are outages
/// and `Degraded` is a service not meeting its check (slow, partial, rate
/// limited). All three open an incident and none counts as recovery — an
/// incident clears only on a sustained run of genuine `Up`. Exhaustive on
/// purpose: a new `CheckStatus` variant must classify here, never default to
/// healthy and silently auto-close incidents.
fn is_bad(status: CheckStatus) -> bool {
    match status {
        CheckStatus::Down | CheckStatus::Error | CheckStatus::Degraded => true,
        CheckStatus::Up => false,
    }
}

fn trailing_bad_run(results: &[CheckResult]) -> &[CheckResult] {
    let split = results
        .iter()
        .rposition(|r| !is_bad(r.status))
        .map(|i| i + 1)
        .unwrap_or(0);
    &results[split..]
}

fn trailing_good_run(results: &[CheckResult]) -> &[CheckResult] {
    let split = results
        .iter()
        .rposition(|r| is_bad(r.status))
        .map(|i| i + 1)
        .unwrap_or(0);
    &results[split..]
}

// ── PostgreSQL implementation ────────────────────────────────────────────────

pub struct PgIncidentStore {
    pool: PgPool,
}

impl PgIncidentStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(FromRow)]
struct OpenIncidentRow {
    id: Uuid,
    target_id: Uuid,
    started_at: DateTime<Utc>,
    region: Option<String>,
}

#[async_trait]
impl IncidentStore for PgIncidentStore {
    async fn open_for_target(&self, org: OrgId, target_id: Uuid) -> Result<Option<OpenIncident>> {
        let row: Option<OpenIncidentRow> = sqlx::query_as::<_, OpenIncidentRow>(
            r#"SELECT id, target_id, started_at, region FROM incidents
               WHERE target_id = $1 AND org_id = $2 AND ended_at IS NULL
               ORDER BY started_at DESC LIMIT 1"#,
        )
        .bind(target_id)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .context("incident open_for_target")?;
        Ok(row.map(|r| OpenIncident {
            id: r.id,
            target_id: r.target_id,
            started_at: r.started_at,
            region: r.region,
        }))
    }

    async fn open_for_pairs(
        &self,
        pairs: &[(OrgId, Uuid)],
    ) -> Result<std::collections::HashMap<(OrgId, Uuid), Vec<OpenIncident>>> {
        if pairs.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        // UNNEST is the supported way to push a `(uuid, uuid)` zip into a
        // single SQL set without ballooning bind count or fanning out to
        // `IN ($1,$2),($3,$4),…`. Joining on the partial index
        // `incidents(org_id, target_id) WHERE ended_at IS NULL` makes this
        // O(page_size) index probes inside one round-trip.
        let (orgs, targets): (Vec<Uuid>, Vec<Uuid>) = pairs.iter().map(|(o, t)| (o.0, *t)).unzip();
        #[derive(FromRow)]
        struct Row {
            org_id: Uuid,
            id: Uuid,
            target_id: Uuid,
            started_at: DateTime<Utc>,
            region: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as::<_, Row>(
            r#"SELECT i.org_id, i.id, i.target_id, i.started_at, i.region
               FROM incidents i
               JOIN unnest($1::uuid[], $2::uuid[]) AS pairs(org_id, target_id)
                 ON i.org_id = pairs.org_id AND i.target_id = pairs.target_id
               WHERE i.ended_at IS NULL"#,
        )
        .bind(&orgs)
        .bind(&targets)
        .fetch_all(&self.pool)
        .await
        .context("incident open_for_pairs")?;
        let mut out: std::collections::HashMap<(OrgId, Uuid), Vec<OpenIncident>> =
            std::collections::HashMap::new();
        for r in rows {
            out.entry((OrgId(r.org_id), r.target_id))
                .or_default()
                .push(OpenIncident {
                    id: r.id,
                    target_id: r.target_id,
                    started_at: r.started_at,
                    region: r.region,
                });
        }
        Ok(out)
    }

    async fn insert_open(&self, org: OrgId, new: NewOpenIncident) -> Result<Option<Uuid>> {
        let status_at_start = status_to_db(new.status_at_start)
            .ok_or_else(|| anyhow::anyhow!("cannot open incident from status=up"))?;
        // ON CONFLICT on the partial unique index is the race-safe single-open
        // guarantee: a concurrent writer yields no row → None → no page.
        // Visibility is derived here, not by the caller: an incident is public
        // only while its monitor is a component of an enabled status page.
        let row: Option<(Uuid,)> = sqlx::query_as(
            r#"INSERT INTO incidents (org_id, target_id, started_at, status_at_start, check_count, error_sample, region, regions_down, regions_up, visibility)
               SELECT $6, $1, $2, $3, $4, $5, $7, $8, $9,
                      CASE WHEN EXISTS (
                          SELECT 1 FROM status_page_components spc
                          JOIN status_pages sp ON sp.id = spc.status_page_id
                          WHERE spc.target_id = $1 AND spc.org_id = $6 AND sp.enabled = true
                      ) THEN 'public' ELSE 'internal' END
               ON CONFLICT (org_id, target_id) WHERE ended_at IS NULL DO NOTHING
               RETURNING id"#,
        )
        .bind(new.target_id)
        .bind(new.started_at)
        .bind(status_at_start)
        .bind(new.check_count as i32)
        .bind(new.error_sample)
        .bind(org.0)
        .bind(new.region)
        .bind(&new.regions_down)
        .bind(&new.regions_up)
        .fetch_optional(&self.pool)
        .await
        .context("incident insert_open")?;
        Ok(row.map(|r| r.0))
    }

    async fn close(&self, org: OrgId, incident_id: Uuid, ended_at: DateTime<Utc>) -> Result<bool> {
        // Recovery: close the row, resolve with no human resolver. For a public
        // incident, append a `resolved` update for the timeline. The UPDATE
        // matches only while ended_at was NULL, so the final SELECT returns a
        // row only for the call that actually closed it — a re-run or the race
        // loser returns None and never re-pages.
        let row: Option<(Uuid,)> = sqlx::query_as(
            r#"WITH closed AS (
                   UPDATE incidents
                      SET ended_at = $2,
                          duration_secs = GREATEST(0, EXTRACT(EPOCH FROM ($2 - started_at))::int),
                          state = 'resolved',
                          resolved_by = NULL,
                          next_escalation_at = NULL,
                          updated_at = now()
                    WHERE id = $1 AND org_id = $3 AND ended_at IS NULL
                   RETURNING id, org_id, visibility
               ),
               ins AS (
                   INSERT INTO incident_updates (org_id, incident_id, phase, message, author)
                   SELECT org_id, id, 'resolved', $4, 'system'
                   FROM closed WHERE visibility = 'public'
               )
               SELECT id FROM closed"#,
        )
        .bind(incident_id)
        .bind(ended_at)
        .bind(org.0)
        .bind(crate::storage::incident_ops::AUTO_RESOLVED_MESSAGE)
        .fetch_optional(&self.pool)
        .await
        .context("incident close")?;
        Ok(row.is_some())
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
    pub region: Option<String>,
    pub regions_down: Vec<String>,
    pub regions_up: Vec<String>,
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
    async fn open_for_pairs(
        &self,
        pairs: &[(OrgId, Uuid)],
    ) -> Result<std::collections::HashMap<(OrgId, Uuid), Vec<OpenIncident>>> {
        let g = self.inner.lock();
        let mut out = std::collections::HashMap::with_capacity(pairs.len());
        for (org, tid) in pairs {
            let Some(rows) = g.by_target.get(tid) else {
                continue;
            };
            let open: Vec<OpenIncident> = rows
                .iter()
                .filter(|i| i.ended_at.is_none())
                .map(|i| OpenIncident {
                    id: i.id,
                    target_id: i.target_id,
                    started_at: i.started_at,
                    region: i.region.clone(),
                })
                .collect();
            if !open.is_empty() {
                out.insert((*org, *tid), open);
            }
        }
        Ok(out)
    }

    async fn open_for_target(&self, _org: OrgId, target_id: Uuid) -> Result<Option<OpenIncident>> {
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
                region: i.region.clone(),
            });
        Ok(open)
    }

    async fn insert_open(&self, _org: OrgId, new: NewOpenIncident) -> Result<Option<Uuid>> {
        let mut g = self.inner.lock();
        let bucket = g.by_target.entry(new.target_id).or_default();
        // Mirrors the DB unique index: a target already holding an open
        // incident yields None so the racer never pages.
        if bucket.iter().any(|i| i.ended_at.is_none()) {
            return Ok(None);
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
            region: new.region,
            regions_down: new.regions_down,
            regions_up: new.regions_up,
        });
        g.inserts += 1;
        Ok(Some(id))
    }

    async fn close(&self, _org: OrgId, incident_id: Uuid, ended_at: DateTime<Utc>) -> Result<bool> {
        let mut g = self.inner.lock();
        let mut closed = false;
        for bucket in g.by_target.values_mut() {
            for inc in bucket.iter_mut() {
                if inc.id == incident_id && inc.ended_at.is_none() {
                    inc.ended_at = Some(ended_at);
                    closed = true;
                }
            }
        }
        if closed {
            g.closes += 1;
        }
        Ok(closed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use chrono::TimeZone;

    use crate::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod, Target, TargetAlerts};
    use crate::storage::admin::EnabledTargetStream;
    use crate::storage::{InMemorySink, InMemoryTargetStore, ResultSink};

    use super::*;

    fn ts(base: DateTime<Utc>, secs: i64) -> DateTime<Utc> {
        base + ChronoDuration::seconds(secs)
    }

    fn result(target_id: Uuid, when: DateTime<Utc>, status: CheckStatus) -> CheckResult {
        CheckResult {
            target_id,
            org_id: Uuid::nil(),
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
            region: None,
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
            region: None,
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
            region: None,
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
    fn decide_isolated_good_blip_does_not_close_then_reopen() {
        // A flapping monitor: while an incident is open, one stray Up between bad
        // checks must not close it — a close would be followed by a reopen on the
        // next bad run, a page storm. Symmetric confirmation (a sustained good run
        // to close, a sustained bad run to reopen) keeps it one incident, so no
        // separate flap cooldown is needed.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            region: None,
            started_at: ts(base, 0),
        };
        let results = vec![
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Up),
            result(target, ts(base, 90), CheckStatus::Down),
            result(target, ts(base, 120), CheckStatus::Down),
        ];
        assert_eq!(decide(Some(&open), &results, 2), Action::None);
    }

    #[test]
    fn decide_degraded_run_opens_incident() {
        // A degraded service is unhealthy: a sustained run opens an incident.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Degraded),
            result(target, ts(base, 30), CheckStatus::Degraded),
            result(target, ts(base, 60), CheckStatus::Degraded),
        ];
        match decide(None, &results, 2) {
            Action::Open(new) => {
                assert_eq!(new.status_at_start, CheckStatus::Degraded);
                assert_eq!(new.check_count, 3);
                assert_eq!(new.started_at, ts(base, 0));
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn decide_degraded_run_does_not_close_open_incident() {
        // Degraded is not recovery; the incident stays open until a clean Up run.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            region: None,
            started_at: ts(base, 0),
        };
        let results = vec![
            result(target, ts(base, 30), CheckStatus::Error),
            result(target, ts(base, 60), CheckStatus::Degraded),
            result(target, ts(base, 90), CheckStatus::Degraded),
        ];
        assert_eq!(decide(Some(&open), &results, 2), Action::None);
    }

    #[test]
    fn decide_trailing_degraded_extends_a_bad_run() {
        // [Down, Down, Degraded] is still three unhealthy checks in a row.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Down),
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Degraded),
        ];
        match decide(None, &results, 2) {
            Action::Open(new) => {
                assert_eq!(new.check_count, 3);
                assert_eq!(new.status_at_start, CheckStatus::Down);
                assert_eq!(new.started_at, ts(base, 0));
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn decide_degraded_within_a_bad_run_carries_count() {
        // [Down, Degraded, Down, Down] is one unbroken unhealthy run of 4.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let results = vec![
            result(target, ts(base, 0), CheckStatus::Down),
            result(target, ts(base, 30), CheckStatus::Degraded),
            result(target, ts(base, 60), CheckStatus::Down),
            result(target, ts(base, 90), CheckStatus::Down),
        ];
        match decide(None, &results, 2) {
            Action::Open(new) => {
                assert_eq!(new.check_count, 4);
                assert_eq!(new.started_at, ts(base, 0));
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn decide_trailing_degraded_does_not_close() {
        // A single Up between bad checks, ending Degraded, is not a recovery run.
        let base = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let target = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: target,
            region: None,
            started_at: ts(base, 0),
        };
        let results = vec![
            result(target, ts(base, 30), CheckStatus::Down),
            result(target, ts(base, 60), CheckStatus::Up),
            result(target, ts(base, 90), CheckStatus::Degraded),
        ];
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
            region: None,
            started_at: ts(base, 0),
        };
        // Same input, but now we know about the open incident; trailing 'up'
        // run length is 0, so nothing happens.
        assert_eq!(decide(Some(&open), &results, 2), Action::None);
    }

    // ── multi-region decide_multi() ─────────────────────────────────────────

    fn mbase() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap()
    }

    #[test]
    fn any_down_opens_from_a_single_bad_region_among_healthy() {
        // The original interleave bug: one region down while another is up. A
        // blended stream could let the healthy region's rows mask it; per-region
        // evaluation opens correctly.
        let b = mbase();
        let t = Uuid::now_v7();
        let by_region = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Down),
                    result(t, ts(b, 30), CheckStatus::Down),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Up),
                    result(t, ts(b, 30), CheckStatus::Up),
                ],
            ),
        ];
        match decide_multi(t, &[], &by_region, 2, 1).as_slice() {
            [Action::Open(n)] => {
                assert_eq!(n.region, None, "combined incident is region-agnostic");
                assert_eq!(n.started_at, ts(b, 0));
                assert_eq!(n.status_at_start, CheckStatus::Down);
            }
            other => panic!("expected one Open, got {other:?}"),
        }
    }

    #[test]
    fn any_down_stays_open_while_one_region_still_bad() {
        let b = mbase();
        let t = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: t,
            started_at: ts(b, 0),
            region: None,
        };
        let by_region = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 30), CheckStatus::Down),
                    result(t, ts(b, 60), CheckStatus::Up),
                    result(t, ts(b, 90), CheckStatus::Up),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 60), CheckStatus::Down),
                    result(t, ts(b, 90), CheckStatus::Down),
                ],
            ),
        ];
        assert!(
            decide_multi(t, &[open], &by_region, 2, 1).is_empty(),
            "must not close while a region is still down"
        );
    }

    #[test]
    fn any_down_closes_when_all_regions_recovered() {
        let b = mbase();
        let t = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: t,
            started_at: ts(b, 0),
            region: None,
        };
        let by_region = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 30), CheckStatus::Down),
                    result(t, ts(b, 60), CheckStatus::Up),
                    result(t, ts(b, 90), CheckStatus::Up),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 120), CheckStatus::Up),
                    result(t, ts(b, 150), CheckStatus::Up),
                ],
            ),
        ];
        match decide_multi(t, std::slice::from_ref(&open), &by_region, 2, 1).as_slice() {
            [
                Action::Close {
                    incident_id,
                    ended_at,
                },
            ] => {
                assert_eq!(*incident_id, open.id);
                // Latest region recovery onset wins.
                assert_eq!(*ended_at, ts(b, 120));
            }
            other => panic!("expected one Close, got {other:?}"),
        }
    }

    #[test]
    fn quorum_needs_two_regions_before_opening() {
        let b = mbase();
        let t = Uuid::now_v7();
        let one_bad = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Down),
                    result(t, ts(b, 30), CheckStatus::Down),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Up),
                    result(t, ts(b, 30), CheckStatus::Up),
                ],
            ),
        ];
        assert!(
            decide_multi(t, &[], &one_bad, 2, 2).is_empty(),
            "one region down is below quorum"
        );

        let two_bad = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Down),
                    result(t, ts(b, 30), CheckStatus::Down),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 60), CheckStatus::Down),
                    result(t, ts(b, 90), CheckStatus::Down),
                ],
            ),
        ];
        match decide_multi(t, &[], &two_bad, 2, 2).as_slice() {
            [Action::Open(n)] => {
                assert_eq!(n.region, None);
                // Opens when the quorum-th (second) region went bad.
                assert_eq!(n.started_at, ts(b, 60));
            }
            other => panic!("expected one Open, got {other:?}"),
        }
    }

    #[test]
    fn quorum_closes_when_back_below_threshold() {
        // One region still down, but below a quorum of 2 → the combined incident
        // clears (the per-region policy is the one that keeps it open).
        let b = mbase();
        let t = Uuid::now_v7();
        let open = OpenIncident {
            id: Uuid::now_v7(),
            target_id: t,
            started_at: ts(b, 0),
            region: None,
        };
        let by_region = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 30), CheckStatus::Down),
                    result(t, ts(b, 60), CheckStatus::Up),
                    result(t, ts(b, 90), CheckStatus::Up),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 60), CheckStatus::Down),
                    result(t, ts(b, 90), CheckStatus::Down),
                ],
            ),
        ];
        match decide_multi(t, std::slice::from_ref(&open), &by_region, 2, 2).as_slice() {
            [Action::Close { incident_id, .. }] => assert_eq!(*incident_id, open.id),
            other => panic!("expected one Close, got {other:?}"),
        }
    }

    #[test]
    fn quorum_clamps_to_live_region_count() {
        // quorum=3 but only 2 regions report → clamps to 2, so a both-down
        // outage still opens instead of waiting for a third region that
        // doesn't exist.
        let b = mbase();
        let t = Uuid::now_v7();
        let by_region = vec![
            (
                "eu".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Down),
                    result(t, ts(b, 30), CheckStatus::Down),
                ],
            ),
            (
                "us".to_string(),
                vec![
                    result(t, ts(b, 0), CheckStatus::Down),
                    result(t, ts(b, 30), CheckStatus::Down),
                ],
            ),
        ];
        match decide_multi(t, &[], &by_region, 2, 3).as_slice() {
            [Action::Open(_)] => {}
            other => panic!("expected Open (quorum clamped to live count), got {other:?}"),
        }
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
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
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
            page_size: 256,
            max_concurrency: 4,
        };
        IncidentWriter::new(
            targets as Arc<dyn EnabledTargetStream>,
            sink as Arc<dyn crate::storage::ResultsStore>,
            incidents as Arc<dyn IncidentStore>,
            cfg,
        )
    }

    fn writer_with_lookback(
        targets: Arc<InMemoryTargetStore>,
        sink: Arc<InMemorySink>,
        incidents: Arc<InMemoryIncidentStore>,
        lookback: ChronoDuration,
    ) -> IncidentWriter {
        let cfg = IncidentWriterConfig {
            tick_interval: StdDuration::from_secs(1),
            lookback,
            flap_threshold: 2,
            max_results_per_tick: 10_000,
            page_size: 256,
            max_concurrency: 4,
        };
        IncidentWriter::new(
            targets as Arc<dyn EnabledTargetStream>,
            sink as Arc<dyn crate::storage::ResultsStore>,
            incidents as Arc<dyn IncidentStore>,
            cfg,
        )
    }

    #[test]
    fn lookback_grows_with_target_interval() {
        let w = writer_with_lookback(
            Arc::new(InMemoryTargetStore::new()),
            Arc::new(InMemorySink::new()),
            Arc::new(InMemoryIncidentStore::new()),
            ChronoDuration::minutes(10),
        );
        let mut fast = make_public_target("fast");
        fast.interval = StdDuration::from_secs(30);
        fast.alert_confirmations = 2;
        assert_eq!(
            w.lookback_for(&fast),
            ChronoDuration::minutes(10),
            "fast monitor is bounded by the floor"
        );
        let mut hourly = make_public_target("cert");
        hourly.interval = StdDuration::from_secs(3600);
        hourly.alert_confirmations = 2;
        assert_eq!(
            w.lookback_for(&hourly),
            ChronoDuration::hours(4),
            "2 * confirmations * interval beats the floor for an hourly monitor"
        );
    }

    #[tokio::test]
    async fn hourly_monitor_opens_despite_small_floor() {
        // tls_cert / domain_expiry are forced to 3600s. Two hourly failures sit
        // far outside a 10-min floor; the per-target 4h window catches both.
        let mut target = make_public_target("cert");
        target.interval = StdDuration::from_secs(3600);
        target.alert_confirmations = 2;
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        seed_results(
            &sink,
            vec![
                result(target_id, now - ChronoDuration::hours(2), CheckStatus::Down),
                result(target_id, now - ChronoDuration::hours(1), CheckStatus::Down),
            ],
        )
        .await;
        let w = writer_with_lookback(
            targets,
            sink,
            incidents.clone(),
            ChronoDuration::minutes(10),
        );
        w.tick_once().await.expect("tick");
        assert_eq!(incidents.insert_count(), 1);
    }

    #[tokio::test]
    async fn slow_user_set_interval_opens_despite_small_floor() {
        // A user can set an interval well above the per-kind floor; the window
        // must follow the configured interval, not a fixed constant.
        let mut target = make_public_target("http-slow");
        target.interval = StdDuration::from_secs(600);
        target.alert_confirmations = 2;
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        // 600s interval → 40-min window; samples at 25 and 15 min are inside it
        // but outside the 10-min floor.
        seed_results(
            &sink,
            vec![
                result(
                    target_id,
                    now - ChronoDuration::minutes(25),
                    CheckStatus::Down,
                ),
                result(
                    target_id,
                    now - ChronoDuration::minutes(15),
                    CheckStatus::Down,
                ),
            ],
        )
        .await;
        let w = writer_with_lookback(
            targets,
            sink,
            incidents.clone(),
            ChronoDuration::minutes(10),
        );
        w.tick_once().await.expect("tick");
        assert_eq!(incidents.insert_count(), 1);
    }

    #[tokio::test]
    async fn fast_monitor_ignores_results_older_than_its_window() {
        // Negative control: a 30s monitor's window is the 10-min floor, so two
        // failures spaced an hour apart fall outside it and must not open —
        // proving the window is interval-scoped, not unbounded.
        let mut target = make_public_target("fast");
        target.interval = StdDuration::from_secs(30);
        target.alert_confirmations = 2;
        let target_id = target.id;
        let now = Utc::now();
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let sink = Arc::new(InMemorySink::new());
        let incidents = Arc::new(InMemoryIncidentStore::new());
        seed_results(
            &sink,
            vec![
                result(target_id, now - ChronoDuration::hours(2), CheckStatus::Down),
                result(target_id, now - ChronoDuration::hours(1), CheckStatus::Down),
            ],
        )
        .await;
        let w = writer_with_lookback(
            targets,
            sink,
            incidents.clone(),
            ChronoDuration::minutes(10),
        );
        w.tick_once().await.expect("tick");
        assert_eq!(incidents.insert_count(), 0);
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
