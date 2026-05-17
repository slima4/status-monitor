use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::api::types::{StatusBreakdown, TagCount, TargetsSummary};
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
    pub tag: Option<String>,
    pub enabled: Option<bool>,
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
    /// Total rows matching `filter` (ignoring `limit`/`offset`). Used to fill
    /// the `total` field of `PageEnvelope`.
    async fn count(&self, org: OrgId, filter: TargetFilter) -> Result<u64>;
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
    /// Total number of distinct tags matching `prefix`.
    async fn count_tags(&self, org: OrgId, prefix: Option<String>) -> Result<u64>;
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
    /// Liveness probe for `/readyz` — connection-level, not tenant data.
    async fn ping(&self) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
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
    /// Total rows for `target_id` in `range`. Used to fill the `total` field
    /// of `PageEnvelope`.
    async fn count_results(&self, org: OrgId, target_id: Uuid, range: TimeRange) -> Result<u64>;
    async fn uptime(&self, org: OrgId, target_id: Uuid, range: TimeRange) -> Result<UptimeStats>;
    /// Coalesce consecutive `down`/`error` results in `range` into incidents.
    /// `ongoing_only` filters out incidents that have already ended.
    async fn list_incidents(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        ongoing_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Incident>>;
    async fn count_incidents(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        ongoing_only: bool,
    ) -> Result<u64>;
    /// Per-status breakdown using each target's most recent observation in
    /// `range`. Targets with no observations in the range are omitted from
    /// the counts.
    async fn current_status_breakdown(
        &self,
        org: OrgId,
        range: TimeRange,
    ) -> Result<StatusBreakdown>;
    /// Aggregate uptime and incident count across all targets in `range`.
    /// Returns `(checks_total, checks_up, incident_count)`.
    async fn last_n_summary(&self, org: OrgId, range: TimeRange) -> Result<(u64, u64, u64)>;
}
