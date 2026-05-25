use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::api::types::{
    DashboardMetrics, DashboardSparkBucket, FleetRibbonBucket, PriorPeriodSummary, StatusBreakdown,
    TagCount, TargetsSummary,
};
use crate::domain::{CheckResult, CheckStatus, Incident, NewTarget, OrgId, Target, TargetUpdate};
use crate::error::Result;

#[async_trait]
pub trait ResultSink: Send + Sync {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()>;
}

#[derive(Debug, Default, Clone)]
pub struct TargetFilter {
    pub limit: Option<usize>,
    pub offset: usize,
    /// ILIKE substring against `name`. Callers pass `None` (not `Some("")`)
    /// for "no filter" — the store does not normalise empty strings.
    pub q: Option<String>,
    pub tag: Option<String>,
    pub enabled: Option<bool>,
    pub group: Option<String>,
    pub owner: Option<Uuid>,
    #[allow(dead_code)]
    pub sort: TargetSort,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TargetSort {
    #[default]
    RecentActivity,
    Name,
    Created,
}

/// Operator-facing target repository. Every method is scoped to a single
/// `org`: the caller resolves it from the request (`CurrentOrg`) and the
/// implementation refuses to touch any other tenant's rows. The `org`
/// parameter is non-optional precisely so "forgot to scope" is a compile
/// error, not a cross-tenant data leak. Scheduler-wide (cross-org)
/// enumeration is a different trait — see `storage::admin::AdminRepo`.
#[async_trait]
pub trait TargetStore: Send + Sync {
    async fn list(&self, org: OrgId, filter: TargetFilter) -> Result<Vec<Target>>;
    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<Target>>;
    /// Create one target. `max_targets` is the plan cap; the INSERT is
    /// guarded by `(count) + 1 <= max_targets` so the bound holds even
    /// against a concurrent create (no check-then-act). Returns
    /// `AppError::QuotaExceeded` when the cap is reached.
    async fn create(&self, org: OrgId, new: NewTarget, max_targets: i64) -> Result<Target>;
    async fn update(&self, org: OrgId, id: Uuid, update: TargetUpdate) -> Result<Option<Target>>;
    async fn delete(&self, org: OrgId, id: Uuid) -> Result<bool>;
    /// Bulk create. Same atomic `(count) + items.len() <= max_targets`
    /// bound; either all rows insert or none do (`AppError::QuotaExceeded`).
    async fn bulk_create(
        &self,
        org: OrgId,
        items: Vec<NewTarget>,
        max_targets: i64,
    ) -> Result<Vec<Target>>;
    async fn list_updated_since(&self, org: OrgId, since: DateTime<Utc>) -> Result<Vec<Target>>;
    /// Aggregate tag inventory across all targets. `prefix` filters tag names
    /// for autocomplete; `limit` caps the number of returned rows. Sorted by
    /// descending count, then alphabetical.
    async fn list_tags(
        &self,
        org: OrgId,
        prefix: Option<String>,
        limit: usize,
    ) -> Result<Vec<TagCount>>;
    /// Totals + enabled/disabled split for the dashboard.
    async fn summary(&self, org: OrgId) -> Result<TargetsSummary>;
    /// Atomically enable or disable each id; returns the set that existed.
    async fn set_enabled(&self, org: OrgId, ids: &[Uuid], enabled: bool) -> Result<Vec<Uuid>>;
    /// Atomically delete each id; returns the set that existed.
    async fn delete_bulk(&self, org: OrgId, ids: &[Uuid]) -> Result<Vec<Uuid>>;
    /// Adds `tags` to every named target; returns the set that existed.
    async fn add_tags(&self, org: OrgId, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>>;
    /// Removes `tags` from every named target; returns the set that existed.
    async fn remove_tags(&self, org: OrgId, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>>;
    /// Sets every named target's `group_name` to `group` (None clears).
    /// Returns the set that existed.
    async fn set_group(&self, org: OrgId, ids: &[Uuid], group: Option<&str>) -> Result<Vec<Uuid>>;
    /// Liveness probe for `/readyz` — connection-level, not tenant data.
    async fn ping(&self) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Per-call options for [`ResultsStore::list_incidents`]. Grouped into a
/// struct so adding a filter (e.g. severity) doesn't widen every impl
/// and caller signature.
///
/// `monitor_interval` lets the storage layer pre-filter to bad statuses
/// at the database (huge wire-cost reduction on healthy monitors) and
/// infer recovery from gaps larger than `2 × interval`. `ongoing_only`
/// drops incidents that already ended.
#[derive(Debug, Clone, Copy)]
pub struct IncidentListQuery {
    pub range: TimeRange,
    pub monitor_interval: std::time::Duration,
    pub ongoing_only: bool,
    pub limit: usize,
    pub offset: usize,
}

impl IncidentListQuery {
    /// First-page convenience for the typical "show me the most recent N
    /// incidents in this window" query (no ongoing filter, no offset).
    pub fn page(range: TimeRange, monitor_interval: std::time::Duration, limit: usize) -> Self {
        Self {
            range,
            monitor_interval,
            ongoing_only: false,
            limit,
            offset: 0,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize, utoipa::ToSchema)]
pub struct UptimeStats {
    pub total: u64,
    pub up: u64,
    pub down: u64,
    pub degraded: u64,
    pub error: u64,
    #[schema(example = 99.94)]
    pub uptime_pct: f64,
}

impl UptimeStats {
    pub fn from_results(results: &[CheckResult]) -> Self {
        let mut stats = Self::default();
        for r in results {
            stats.total += 1;
            match r.status {
                CheckStatus::Up => stats.up += 1,
                CheckStatus::Down => stats.down += 1,
                CheckStatus::Degraded => stats.degraded += 1,
                CheckStatus::Error => stats.error += 1,
            }
        }
        if stats.total > 0 {
            stats.uptime_pct = (stats.up as f64 / stats.total as f64) * 100.0;
        }
        stats
    }
}

/// Operator-facing results repository. Org-scoped on every method for the
/// same reason as [`TargetStore`]: the `org` is resolved from the request and
/// the implementation never returns another tenant's check history. A bare
/// `target_id` is not enough — a UUID guessed from another org must resolve to
/// zero rows, not that org's data.
#[async_trait]
pub trait ResultsStore: Send + Sync {
    async fn list_results(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckResult>>;
    async fn uptime(&self, org: OrgId, target_id: Uuid, range: TimeRange) -> Result<UptimeStats>;
    /// Coalesce consecutive `down`/`error` results in the requested window
    /// into incidents. See [`IncidentListQuery`] for the per-call options.
    async fn list_incidents(
        &self,
        org: OrgId,
        target_id: Uuid,
        query: IncidentListQuery,
    ) -> Result<Vec<Incident>>;
    /// Per-status breakdown using each target's most recent observation in
    /// `range`. Targets with no observations in the range are omitted from
    /// the counts.
    async fn current_status_breakdown(
        &self,
        org: OrgId,
        range: TimeRange,
    ) -> Result<StatusBreakdown>;
    /// Aggregate uptime, response, and incident count across all targets
    /// in `range`. Returns `(checks_total, checks_up, avg_ms,
    /// incident_count)`. `avg_ms` is the true sample-weighted mean —
    /// must come from the same source as `checks_total`/`checks_up` so
    /// the dashboard's Δ-vs-prior comparison doesn't mix raw and
    /// per-target-rounded numbers.
    async fn last_n_summary(&self, org: OrgId, range: TimeRange) -> Result<(u64, u64, u32, u64)>;
    /// Per-monitor rollup for the operator dashboard table. One row per
    /// target with samples/up/p50/p95/last_status in `range`. Targets
    /// with no samples are omitted; the caller joins the result with the
    /// target list and synthesises a zero-sample row when needed.
    async fn dashboard_rollup(&self, org: OrgId, range: TimeRange)
    -> Result<Vec<DashboardMetrics>>;
    /// Minute-bucketed average duration for the last 60 minutes, every
    /// monitor in the org. Drives the per-row sparkline. Implementations
    /// MUST read from the `check_results_1m` rollup (already aggregated
    /// per minute) so the cost stays cheap even for large orgs.
    async fn dashboard_sparkline(
        &self,
        org: OrgId,
        from: chrono::DateTime<Utc>,
        to: chrono::DateTime<Utc>,
    ) -> Result<Vec<DashboardSparkBucket>>;
    /// Fleet-wide uptime ribbon: one bucket per `bucket_seconds` slice
    /// across every monitor in `org`. Implementations MUST read from the
    /// `check_results_1m` rollup so the cost stays O(buckets) regardless
    /// of monitor count. Buckets with no samples are omitted; the caller
    /// fills the window into a fixed-length array.
    async fn fleet_ribbon(
        &self,
        org: OrgId,
        from: chrono::DateTime<Utc>,
        to: chrono::DateTime<Utc>,
        bucket_seconds: u32,
    ) -> Result<Vec<FleetRibbonBucket>>;
    /// Fleet totals for the period immediately preceding `range` (same
    /// span, ending at `range.from`). Drives the Δ-vs-prior hints on
    /// each KPI card. Implementations MUST read from the same source as
    /// `last_n_summary` so the dashboard never compares matview-derived
    /// totals against raw totals (ingest lag would produce phantom
    /// deltas). Returns zeros when there is no prior data — the view
    /// layer hides the hint in that case.
    async fn prior_period_summary(
        &self,
        org: OrgId,
        range: TimeRange,
    ) -> Result<PriorPeriodSummary>;
}
