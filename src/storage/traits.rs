use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::api::types::{
    AvailabilityBucket, DashboardMetrics, DashboardSparkBucket, FleetRibbonBucket, LatencyBucket,
    PriorPeriodSummary, RegionLatencySeries, RegionRollup, StatusBreakdown, TagCount,
    TargetsSummary,
};
use crate::domain::{
    CheckResult, CheckStatus, NewTarget, OrgId, Target, TargetUpdate, WriteSource,
};
use crate::error::Result;

#[async_trait]
pub trait ResultSink: Send + Sync {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()>;

    /// Write results tagged with the producing region + agent. The default
    /// ignores the tags (home path); CH overrides to stamp them. Used by the
    /// agent ingest API to attribute results to the submitting region.
    async fn write_batch_tagged(
        &self,
        results: &[CheckResult],
        _region: &str,
        _agent_id: &str,
    ) -> Result<()> {
        self.write_batch(results).await
    }
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
    /// `check_spec` type tag (`http`/`tcp`/`dns`/`tls_cert`/`domain_expiry`).
    pub kind: Option<String>,
    /// Restrict to targets that run in this region (via `target_regions`).
    pub region: Option<String>,
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
    /// `check kind → count` across the whole org for the given filter, with the
    /// filter's own `kind` ignored. Powers the type-chip tallies so they stay
    /// org-wide and invariant when switching chips (not page-scoped).
    async fn count_by_kind(
        &self,
        org: OrgId,
        filter: TargetFilter,
    ) -> Result<std::collections::HashMap<String, i64>>;
    /// Distinct non-null `group_name`s across the org, sorted. Powers the
    /// group filter dropdown so its options are org-wide, not page-scoped.
    async fn distinct_groups(&self, org: OrgId) -> Result<Vec<String>>;
    /// `id → name` for every live target in the org. A projection: it skips the
    /// `check_spec` decode + secret decrypt that `list` pays, for callers that
    /// only need labels (incident console, reports).
    async fn names(&self, org: OrgId) -> Result<std::collections::HashMap<Uuid, String>>;
    /// `id → (name, check kind)` for every live target in the org. Like
    /// [`names`](Self::names) but also returns the check discriminator
    /// (`http`/`tcp`/`dns`/`tls_cert`/`domain_expiry`), read straight from the
    /// `check_spec` tag — no full spec decode or secret decrypt.
    async fn names_and_kinds(
        &self,
        org: OrgId,
    ) -> Result<std::collections::HashMap<Uuid, (String, String)>>;
    /// Create one target. `max_targets` is the plan cap; the INSERT is
    /// guarded by `(count) + 1 <= max_targets` so the bound holds even
    /// against a concurrent create (no check-then-act). Returns
    /// `AppError::QuotaExceeded` when the cap is reached.
    async fn create(
        &self,
        org: OrgId,
        new: NewTarget,
        source: WriteSource,
        max_targets: i64,
    ) -> Result<Target>;
    async fn update(
        &self,
        org: OrgId,
        id: Uuid,
        update: TargetUpdate,
        source: WriteSource,
    ) -> Result<Option<Target>>;
    async fn delete(&self, org: OrgId, id: Uuid) -> Result<bool>;
    /// Remove every alert binding to `channel_id` across the org's targets.
    /// Channel deletion calls this so a dangling binding can't poison later
    /// whole-array alert updates. Returns the number of targets touched.
    async fn unbind_channel(&self, org: OrgId, channel_id: Uuid) -> Result<u64>;
    /// Bulk create. Same atomic `(count) + items.len() <= max_targets`
    /// bound; either all rows insert or none do (`AppError::QuotaExceeded`).
    async fn bulk_create(
        &self,
        org: OrgId,
        items: Vec<NewTarget>,
        source: WriteSource,
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
    /// Distinct regions this org's targets are assigned to, sorted, for the
    /// dashboard region selector. Empty or single-element means a single-region
    /// org. Reads `target_regions` (config), not check history.
    async fn regions_for_org(&self, org: OrgId) -> Result<Vec<String>>;
    /// Region catalog: ids of every enabled region, sorted. The set a monitor
    /// may be assigned to. Global (regions are operator-defined), so no `org`.
    async fn available_regions(&self) -> Result<Vec<String>>;
    /// Same catalog with display fields for the assignment picker, so the UI
    /// can show a human name + location instead of the bare id.
    async fn available_regions_detailed(&self) -> Result<Vec<RegionOption>>;
    /// Regions one target is assigned to, sorted. `None` if the target is not in
    /// the org (so a guessed id from another tenant reads as not-found).
    async fn regions_for_target(&self, org: OrgId, target_id: Uuid) -> Result<Option<Vec<String>>>;
    /// Replace a target's region assignments with `regions`. `false` if the
    /// target is not in the org. Caller validates the regions exist + are
    /// enabled and within the plan's `max_regions`.
    async fn set_target_regions(
        &self,
        org: OrgId,
        target_id: Uuid,
        regions: &[String],
    ) -> Result<bool>;
}

/// A region for the monitor assignment picker: id plus display fields.
/// `name`/`city` are empty when unset; the UI shows `name` (falling back to
/// `id`) and appends `city` when present.
#[derive(Debug, Clone)]
pub struct RegionOption {
    pub id: String,
    pub name: String,
    pub city: String,
    /// ISO 3166-1 alpha-2 country, when set — drives the flag emoji.
    pub country_code: Option<String>,
    /// Continent slug (see `domain::region::Continent`) for picker grouping.
    pub continent: Option<String>,
    /// Coordinates for a future map; both set or both `None`.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

impl RegionOption {
    /// Catalog display name: `name`, falling back to the id when unset or
    /// auto-seeded equal to the id (the control-plane region).
    pub fn display_name(&self) -> &str {
        let name = self.name.trim();
        if name.is_empty() || name == self.id {
            &self.id
        } else {
            name
        }
    }

    /// Unicode flag emoji for `country_code`, or `None` when unset/invalid.
    pub fn flag(&self) -> Option<String> {
        crate::domain::region::country_flag(self.country_code.as_deref()?)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// A [`TimeRange`] that passed a retention decision. The tenant read methods
/// take this, not a bare `TimeRange`, so the compiler refuses any read that
/// didn't pick a window — closing the per-surface bypass class.
#[derive(Debug, Clone, Copy)]
pub struct ClampedRange(TimeRange);

impl ClampedRange {
    /// Clamp `from` forward to `window_days` before `now`. `window_days` comes
    /// from the plan, keeping storage free of the plan model.
    pub fn for_window(range: TimeRange, window_days: i64, now: DateTime<Utc>) -> Self {
        let floor = now - Duration::try_days(window_days.max(0)).unwrap_or_default();
        Self(TimeRange {
            from: range.from.max(floor),
            to: range.to,
        })
    }

    /// System read that must see the full window (incident detection, building
    /// the public view) — explicit opt-out from the clamp.
    pub fn unclamped(range: TimeRange) -> Self {
        Self(range)
    }

    pub fn inner(&self) -> TimeRange {
        self.0
    }
}

impl std::ops::Deref for ClampedRange {
    type Target = TimeRange;
    fn deref(&self) -> &TimeRange {
        &self.0
    }
}

/// Round a requested bucket width up to a whole number of 60s rollup rows, so
/// every output bucket spans an integer count of minutes (grid alignment is the
/// caller's `toStartOfInterval` / `div_euclid`).
pub fn rollup_bucket_secs(bucket_seconds: u32) -> u32 {
    bucket_seconds.max(60).div_ceil(60) * 60
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
    /// Liveness probe for `/readyz` — connection-level, not tenant data.
    async fn ping(&self) -> Result<()>;
    async fn list_results(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        limit: usize,
        offset: usize,
        region: Option<&str>,
    ) -> Result<Vec<CheckResult>>;
    /// Recent results for one target, each paired with its region, in one query.
    /// The caller groups by region so per-region runs don't interleave.
    async fn list_results_by_region(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(String, CheckResult)>>;
    /// Recent results for many targets in one query, each row tagged with its
    /// region (the `CheckResult` carries `target_id` and `org_id`). Returns at
    /// most `per_target_limit` newest rows per `(target, region)`. Rows are
    /// filtered to the caller's `(org, target)` pairs, so a target id resolving
    /// to another org yields nothing.
    async fn recent_results_for_targets(
        &self,
        targets: &[(OrgId, Uuid)],
        range: ClampedRange,
        per_target_limit: usize,
    ) -> Result<Vec<(String, CheckResult)>>;
    async fn uptime(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        region: Option<&str>,
    ) -> Result<UptimeStats>;
    /// Per-status breakdown using each target's most recent observation in
    /// `range`. Targets with no observations in the range are omitted from
    /// the counts.
    async fn current_status_breakdown(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<StatusBreakdown>;
    /// Aggregate uptime, response, and incident count across all targets
    /// in `range`. Returns `(checks_total, checks_up, avg_ms,
    /// incident_count)`. `avg_ms` is the true sample-weighted mean —
    /// must come from the same source as `checks_total`/`checks_up` so
    /// the dashboard's Δ-vs-prior comparison doesn't mix raw and
    /// per-target-rounded numbers.
    async fn last_n_summary(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<(u64, u64, u32, u64)>;
    /// Per-monitor rollup for the operator dashboard table. One row per
    /// target with samples/up/p50/p95/last_status in `range`. Targets
    /// with no samples are omitted; the caller joins the result with the
    /// target list and synthesises a zero-sample row when needed.
    async fn dashboard_rollup(
        &self,
        org: OrgId,
        range: TimeRange,
        region: Option<&str>,
    ) -> Result<Vec<DashboardMetrics>>;
    /// Minute-bucketed average duration for the last 60 minutes, every
    /// monitor in the org. Drives the per-row sparkline. Implementations
    /// MUST read from the `check_results_1m` rollup (already aggregated
    /// per minute) so the cost stays cheap even for large orgs.
    async fn dashboard_sparkline(
        &self,
        org: OrgId,
        from: chrono::DateTime<Utc>,
        to: chrono::DateTime<Utc>,
        region: Option<&str>,
    ) -> Result<Vec<DashboardSparkBucket>>;
    /// Bucketed latency for a single monitor: p50/p95/p99 + per-phase means
    /// per `bucket_seconds` slice across `range`. Drives the monitor-detail
    /// latency and breakdown charts. Implementations MUST read from the
    /// `check_results_1m` rollup so a 30d window stays O(buckets) regardless
    /// of sample rate. Buckets with no samples are omitted; the chart leaves
    /// the gap unconnected.
    async fn latency_buckets(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
        region: Option<&str>,
    ) -> Result<Vec<LatencyBucket>>;
    /// Per-target up/total counts per bucket — drives the uptime-card
    /// sparkline. Same rollup source and bucketing as [`latency_buckets`](Self::latency_buckets).
    async fn availability_buckets(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
        region: Option<&str>,
    ) -> Result<Vec<AvailabilityBucket>>;
    /// Per-region rollup for one target over `range` — drives the detail-page
    /// region breakdown table. One row per region; regions with no samples are
    /// omitted. Single-region orgs see one row.
    async fn region_breakdown(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
    ) -> Result<Vec<RegionRollup>>;
    /// Per-region latency buckets for one target — the overlay view, each
    /// region a separate line. Same rollup source + bucketing as
    /// [`latency_buckets`](Self::latency_buckets), split by region.
    async fn latency_buckets_by_region(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
    ) -> Result<Vec<RegionLatencySeries>>;
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
        region: Option<&str>,
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
        region: Option<&str>,
    ) -> Result<PriorPeriodSummary>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn for_window_narrows_out_of_window_from() {
        let now = at(2026, 5, 1);
        let r = ClampedRange::for_window(
            TimeRange {
                from: now - Duration::try_days(60).unwrap(),
                to: now,
            },
            30,
            now,
        );
        assert_eq!(r.from, now - Duration::try_days(30).unwrap());
        assert_eq!(r.to, now);
    }

    #[test]
    fn for_window_leaves_in_window_from() {
        let now = at(2026, 5, 1);
        let from = now - Duration::try_days(7).unwrap();
        let r = ClampedRange::for_window(TimeRange { from, to: now }, 30, now);
        assert_eq!(r.from, from);
    }

    #[test]
    fn unclamped_preserves_range() {
        let now = at(2026, 5, 1);
        let from = now - Duration::try_days(400).unwrap();
        let r = ClampedRange::unclamped(TimeRange { from, to: now });
        assert_eq!(r.from, from);
    }
}
