use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::api::types::{
    DashboardMetrics, DashboardSparkBucket, FleetRibbonBucket, LatencyBucket, PriorPeriodSummary,
    StatusBreakdown, TagCount, TargetsSummary,
};
use crate::domain::{
    CheckResult, CheckStatus, Incident, NewTarget, OrgId, Target, TargetUpdate, WriteSource,
    coalesce_incidents,
};
use crate::error::Result;
use crate::storage::traits::{
    ClampedRange, IncidentListQuery, ResultSink, ResultsStore, TargetFilter, TargetStore,
    TimeRange, UptimeStats,
};

#[derive(Default)]
pub struct InMemorySink {
    results: Mutex<Vec<CheckResult>>,
}

impl InMemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<CheckResult> {
        self.results.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.results.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.results.lock().is_empty()
    }
}

#[async_trait]
impl ResultSink for InMemorySink {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()> {
        self.results.lock().extend_from_slice(results);
        Ok(())
    }
}

// Single-org test fixture. The `org` parameter exists to satisfy the
// org-scoped trait (and to type-check the call sites that thread
// `CurrentOrg`), but is intentionally not used for filtering: these stores
// only ever hold one tenant's data in tests. Real cross-tenant isolation
// lives in the Postgres/ClickHouse impls and is covered by the two-org
// integration tests.
#[async_trait]
impl ResultsStore for InMemorySink {
    async fn list_results(
        &self,
        _org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckResult>> {
        let guard = self.results.lock();
        let mut out: Vec<CheckResult> = guard
            .iter()
            .filter(|r| {
                r.target_id == target_id && r.timestamp >= range.from && r.timestamp < range.to
            })
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
        let mut paged: Vec<CheckResult> = out.into_iter().skip(offset).collect();
        if paged.len() > limit {
            paged.truncate(limit);
        }
        Ok(paged)
    }

    async fn uptime(
        &self,
        _org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
    ) -> Result<UptimeStats> {
        let guard = self.results.lock();
        let filtered: Vec<CheckResult> = guard
            .iter()
            .filter(|r| {
                r.target_id == target_id && r.timestamp >= range.from && r.timestamp < range.to
            })
            .cloned()
            .collect();
        Ok(UptimeStats::from_results(&filtered))
    }

    async fn list_incidents(
        &self,
        _org: OrgId,
        target_id: Uuid,
        query: IncidentListQuery,
    ) -> Result<Vec<Incident>> {
        let mut incidents = coalesce_for_target(&self.snapshot(), target_id, query.range.inner());
        if query.ongoing_only {
            incidents.retain(|i| i.ended_at.is_none());
        }
        incidents.sort_by_key(|i| std::cmp::Reverse(i.started_at));
        let paged: Vec<Incident> = incidents
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .collect();
        Ok(paged)
    }

    async fn current_status_breakdown(
        &self,
        _org: OrgId,
        range: TimeRange,
    ) -> Result<StatusBreakdown> {
        let guard = self.results.lock();
        let mut latest: std::collections::HashMap<Uuid, &CheckResult> =
            std::collections::HashMap::new();
        for r in guard.iter() {
            if r.timestamp < range.from || r.timestamp >= range.to {
                continue;
            }
            latest
                .entry(r.target_id)
                .and_modify(|cur| {
                    if r.timestamp > cur.timestamp {
                        *cur = r;
                    }
                })
                .or_insert(r);
        }
        let mut out = StatusBreakdown::default();
        for r in latest.values() {
            match r.status {
                CheckStatus::Up => out.up += 1,
                CheckStatus::Down => out.down += 1,
                CheckStatus::Degraded => out.degraded += 1,
                CheckStatus::Error => out.error += 1,
            }
        }
        Ok(out)
    }

    async fn dashboard_rollup(
        &self,
        _org: OrgId,
        range: TimeRange,
    ) -> Result<Vec<DashboardMetrics>> {
        let guard = self.results.lock();
        let mut by_target: std::collections::BTreeMap<Uuid, Vec<&CheckResult>> =
            std::collections::BTreeMap::new();
        for r in guard.iter() {
            if r.timestamp < range.from || r.timestamp >= range.to {
                continue;
            }
            by_target.entry(r.target_id).or_default().push(r);
        }
        let mut out = Vec::with_capacity(by_target.len());
        for (tid, mut rows) in by_target {
            rows.sort_by_key(|r| r.timestamp);
            let samples = rows.len() as u64;
            let up = rows.iter().filter(|r| r.status == CheckStatus::Up).count() as u64;
            let mut durations: Vec<u32> = rows.iter().map(|r| r.duration_ms).collect();
            durations.sort_unstable();
            let p50_ms = percentile(&durations, 0.50);
            let p95_ms = percentile(&durations, 0.95);
            let avg_ms = durations
                .iter()
                .map(|d| *d as u64)
                .sum::<u64>()
                .checked_div(samples)
                .map_or(0, |v| v.min(u32::MAX as u64) as u32);
            let last_status = rows
                .last()
                .map(|r| r.status.as_str().to_string())
                .unwrap_or_default();
            let last_minute_ts = rows.last().map(|r| {
                let secs = r.timestamp.timestamp().max(0);
                secs - (secs % 60)
            });
            out.push(DashboardMetrics {
                target_id: tid,
                samples,
                up,
                avg_ms,
                p50_ms,
                p95_ms,
                last_status,
                last_minute_ts,
            });
        }
        Ok(out)
    }

    async fn dashboard_sparkline(
        &self,
        _org: OrgId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<DashboardSparkBucket>> {
        let guard = self.results.lock();
        let mut buckets: std::collections::BTreeMap<(Uuid, i64), (u64, u64)> =
            std::collections::BTreeMap::new();
        for r in guard.iter() {
            if r.timestamp < from || r.timestamp >= to {
                continue;
            }
            let minute_ts = (r.timestamp.timestamp() / 60) * 60;
            let slot = buckets.entry((r.target_id, minute_ts)).or_insert((0, 0));
            slot.0 += r.duration_ms as u64;
            slot.1 += 1;
        }
        Ok(buckets
            .into_iter()
            .map(|((target_id, bucket_ts), (sum, n))| DashboardSparkBucket {
                target_id,
                bucket_ts,
                avg_ms: (sum as f32) / (n.max(1) as f32),
            })
            .collect())
    }

    async fn prior_period_summary(
        &self,
        _org: OrgId,
        range: TimeRange,
    ) -> Result<PriorPeriodSummary> {
        let guard = self.results.lock();
        let span = range.to - range.from;
        let prior_to = range.from;
        let prior_from = prior_to - span;
        let mut total = 0u64;
        let mut up = 0u64;
        let mut sum_ms: u64 = 0;
        for r in guard.iter() {
            if r.timestamp < prior_from || r.timestamp >= prior_to {
                continue;
            }
            total += 1;
            sum_ms = sum_ms.saturating_add(r.duration_ms as u64);
            if r.status == CheckStatus::Up {
                up += 1;
            }
        }
        let avg_ms = sum_ms
            .checked_div(total)
            .map_or(0, |v| v.min(u32::MAX as u64) as u32);
        Ok(PriorPeriodSummary {
            checks_total: total,
            checks_up: up,
            avg_ms,
        })
    }

    async fn latency_buckets(
        &self,
        _org: OrgId,
        target_id: Uuid,
        range: ClampedRange,
        bucket_seconds: u32,
    ) -> Result<Vec<LatencyBucket>> {
        #[derive(Default)]
        struct Acc {
            durations: Vec<u32>,
            total_sum: u64,
            // (sum, count) per phase; count skips NULL samples to match the
            // matview's `avgState(<nullable>)` semantics.
            dns: (u64, u64),
            connect: (u64, u64),
            tls: (u64, u64),
            ttfb: (u64, u64),
        }
        fn add(acc: &mut (u64, u64), v: Option<u16>) {
            if let Some(v) = v {
                acc.0 += u64::from(v);
                acc.1 += 1;
            }
        }
        let mean = |(sum, n): (u64, u64)| -> u32 {
            sum.checked_div(n)
                .map_or(0, |v| v.min(u32::MAX as u64) as u32)
        };
        // Round to a whole-minute grain exactly as the ClickHouse impl does,
        // so the in-memory test backend buckets identically to production.
        let bucket = i64::from(bucket_seconds.max(60).div_ceil(60) * 60);
        let mut by_bucket: std::collections::BTreeMap<i64, Acc> = std::collections::BTreeMap::new();
        let guard = self.results.lock();
        for r in guard.iter() {
            if r.target_id != target_id || r.timestamp < range.from || r.timestamp >= range.to {
                continue;
            }
            let slot = (r.timestamp.timestamp() / bucket) * bucket;
            let acc = by_bucket.entry(slot).or_default();
            acc.durations.push(r.duration_ms);
            acc.total_sum += u64::from(r.duration_ms);
            add(&mut acc.dns, r.dns_ms);
            add(&mut acc.connect, r.connect_ms);
            add(&mut acc.tls, r.tls_ms);
            add(&mut acc.ttfb, r.ttfb_ms);
        }
        Ok(by_bucket
            .into_iter()
            .map(|(bucket_ts, mut acc)| {
                acc.durations.sort_unstable();
                let samples = acc.durations.len() as u64;
                LatencyBucket {
                    t: bucket_ts * 1000,
                    p50: percentile(&acc.durations, 0.50),
                    p95: percentile(&acc.durations, 0.95),
                    p99: percentile(&acc.durations, 0.99),
                    avg: mean((acc.total_sum, samples)),
                    dns: mean(acc.dns),
                    connect: mean(acc.connect),
                    tls: mean(acc.tls),
                    ttfb: mean(acc.ttfb),
                    samples,
                }
            })
            .collect())
    }

    async fn fleet_ribbon(
        &self,
        _org: OrgId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket_seconds: u32,
    ) -> Result<Vec<FleetRibbonBucket>> {
        let guard = self.results.lock();
        let bucket = bucket_seconds.max(60) as i64;
        let mut by_bucket: std::collections::BTreeMap<i64, (u64, u64)> =
            std::collections::BTreeMap::new();
        for r in guard.iter() {
            if r.timestamp < from || r.timestamp >= to {
                continue;
            }
            let slot = (r.timestamp.timestamp() / bucket) * bucket;
            let entry = by_bucket.entry(slot).or_insert((0, 0));
            entry.0 += 1;
            if r.status == CheckStatus::Up {
                entry.1 += 1;
            }
        }
        Ok(by_bucket
            .into_iter()
            .map(|(bucket_ts, (samples, up))| FleetRibbonBucket {
                bucket_ts,
                samples,
                up,
            })
            .collect())
    }

    async fn last_n_summary(&self, _org: OrgId, range: TimeRange) -> Result<(u64, u64, u32, u64)> {
        let guard = self.results.lock();
        let mut total = 0u64;
        let mut up = 0u64;
        let mut sum_ms: u64 = 0;
        let mut by_target: std::collections::HashMap<Uuid, Vec<&CheckResult>> =
            std::collections::HashMap::new();
        for r in guard.iter() {
            if r.timestamp < range.from || r.timestamp >= range.to {
                continue;
            }
            total += 1;
            sum_ms = sum_ms.saturating_add(r.duration_ms as u64);
            if r.status == CheckStatus::Up {
                up += 1;
            }
            by_target.entry(r.target_id).or_default().push(r);
        }
        let avg_ms = sum_ms
            .checked_div(total)
            .map_or(0, |v| v.min(u32::MAX as u64) as u32);
        let mut incidents = 0u64;
        for results in by_target.values_mut() {
            results.sort_by_key(|r| r.timestamp);
            let mut in_incident = false;
            for r in results.iter() {
                let bad = matches!(r.status, CheckStatus::Down | CheckStatus::Error);
                if bad && !in_incident {
                    incidents += 1;
                    in_incident = true;
                } else if !bad {
                    in_incident = false;
                }
            }
        }
        Ok((total, up, avg_ms, incidents))
    }
}

/// Nearest-rank percentile on a pre-sorted slice. Mirrors ClickHouse's
/// `quantile(p)` so the in-memory fixture is byte-comparable to the
/// production CH impl. Empty / single-element edge cases collapse to the
/// natural answer (0 / the only value).
fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let idx = ((p * n as f64).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1);
    sorted[idx]
}

fn coalesce_for_target(all: &[CheckResult], target_id: Uuid, range: TimeRange) -> Vec<Incident> {
    let mut filtered: Vec<&CheckResult> = all
        .iter()
        .filter(|r| r.target_id == target_id && r.timestamp >= range.from && r.timestamp < range.to)
        .collect();
    filtered.sort_by_key(|r| r.timestamp);
    coalesce_incidents(
        target_id,
        filtered
            .into_iter()
            .map(|r| (r.timestamp, r.status, r.error.clone())),
    )
}

#[derive(Default)]
pub struct InMemoryTargetStore {
    targets: Mutex<Vec<Target>>,
}

impl InMemoryTargetStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_vec(targets: Vec<Target>) -> Self {
        Self {
            targets: Mutex::new(targets),
        }
    }

    pub fn insert(&self, target: Target) {
        self.targets.lock().push(target);
    }

    fn materialize(new: NewTarget, source: WriteSource) -> Target {
        let now = Utc::now();
        Target {
            id: Uuid::now_v7(),
            name: new.name,
            check: new.check,
            interval: new.interval,
            enabled: new.enabled,
            tags: new.tags,
            alerts: new.alerts,
            group_name: new.group_name,
            owner_user_id: new.owner_user_id,
            write_source: source,
            created_at: now,
            updated_at: now,
        }
    }
}

// Single-org test fixture — see the note on the `ResultsStore` impl above.
// The `org` parameter is accepted to satisfy the trait but not used for
// filtering.
#[async_trait]
impl TargetStore for InMemoryTargetStore {
    async fn list(&self, _org: OrgId, filter: TargetFilter) -> Result<Vec<Target>> {
        let limit = filter.limit.unwrap_or(100).min(10_000);
        let guard = self.targets.lock();
        let q_lower = filter
            .q
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
        let group = filter
            .group
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut collected: Vec<Target> = guard
            .iter()
            .filter(|t| filter.enabled.map(|e| t.enabled == e).unwrap_or(true))
            .filter(|t| match &filter.tag {
                Some(tag) => t.tags.iter().any(|x| x == tag),
                None => true,
            })
            .filter(|t| match &q_lower {
                Some(q) => t.name.to_lowercase().contains(q),
                None => true,
            })
            .filter(|t| match group {
                Some(g) => t.group_name.as_deref() == Some(g),
                None => true,
            })
            .filter(|t| match filter.owner {
                Some(u) => t.owner_user_id == Some(u),
                None => true,
            })
            .cloned()
            .collect();
        match filter.sort {
            crate::storage::traits::TargetSort::RecentActivity => {
                collected.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
            }
            crate::storage::traits::TargetSort::Name => {
                collected.sort_by(|a, b| a.name.cmp(&b.name));
            }
            crate::storage::traits::TargetSort::Created => {
                collected.sort_by_key(|t| t.created_at);
            }
        }
        Ok(collected
            .into_iter()
            .skip(filter.offset)
            .take(limit)
            .collect())
    }

    async fn get(&self, _org: OrgId, id: Uuid) -> Result<Option<Target>> {
        Ok(self.targets.lock().iter().find(|t| t.id == id).cloned())
    }

    async fn create(
        &self,
        _org: OrgId,
        new: NewTarget,
        source: WriteSource,
        max_targets: i64,
    ) -> Result<Target> {
        let mut guard = self.targets.lock();
        // Lock held across count + push, so this is atomic for the same
        // reason the Postgres count-in-INSERT is.
        if guard.len() as i64 + 1 > max_targets {
            return Err(crate::error::AppError::quota_exceeded(
                "max_targets",
                guard.len() as i64,
                max_targets,
                "free",
            ));
        }
        let target = Self::materialize(new, source);
        guard.push(target.clone());
        Ok(target)
    }

    async fn update(
        &self,
        _org: OrgId,
        id: Uuid,
        update: TargetUpdate,
        source: WriteSource,
    ) -> Result<Option<Target>> {
        let mut guard = self.targets.lock();
        let Some(t) = guard.iter_mut().find(|t| t.id == id) else {
            return Ok(None);
        };
        if let Some(n) = update.name {
            t.name = n;
        }
        if let Some(c) = update.check {
            t.check = c;
        }
        if let Some(i) = update.interval {
            t.interval = i;
        }
        if let Some(e) = update.enabled {
            t.enabled = e;
        }
        if let Some(tags) = update.tags {
            t.tags = tags;
        }
        if let Some(alerts) = update.alerts {
            t.alerts = alerts;
        }
        if let Some(v) = update.group_name {
            t.group_name = v;
        }
        if let Some(v) = update.owner_user_id {
            t.owner_user_id = v;
        }
        t.write_source = source;
        t.updated_at = Utc::now();
        Ok(Some(t.clone()))
    }

    async fn delete(&self, _org: OrgId, id: Uuid) -> Result<bool> {
        let mut guard = self.targets.lock();
        let before = guard.len();
        guard.retain(|t| t.id != id);
        Ok(guard.len() != before)
    }

    async fn bulk_create(
        &self,
        _org: OrgId,
        items: Vec<NewTarget>,
        source: WriteSource,
        max_targets: i64,
    ) -> Result<Vec<Target>> {
        let mut guard = self.targets.lock();
        if guard.len() as i64 + items.len() as i64 > max_targets {
            return Err(crate::error::AppError::quota_exceeded(
                "max_targets",
                guard.len() as i64,
                max_targets,
                "free",
            ));
        }
        let mut created = Vec::with_capacity(items.len());
        for new in items {
            let target = Self::materialize(new, source);
            guard.push(target.clone());
            created.push(target);
        }
        Ok(created)
    }

    async fn list_updated_since(&self, _org: OrgId, since: DateTime<Utc>) -> Result<Vec<Target>> {
        Ok(self
            .targets
            .lock()
            .iter()
            .filter(|t| t.updated_at > since)
            .cloned()
            .collect())
    }

    async fn list_tags(
        &self,
        _org: OrgId,
        prefix: Option<String>,
        limit: usize,
    ) -> Result<Vec<TagCount>> {
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for t in self.targets.lock().iter() {
            for tag in &t.tags {
                if let Some(pfx) = prefix.as_deref()
                    && !tag.starts_with(pfx)
                {
                    continue;
                }
                *counts.entry(tag.clone()).or_default() += 1;
            }
        }
        let mut out: Vec<TagCount> = counts
            .into_iter()
            .map(|(name, count)| TagCount { name, count })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
        out.truncate(limit);
        Ok(out)
    }

    async fn summary(&self, _org: OrgId) -> Result<TargetsSummary> {
        let guard = self.targets.lock();
        let total = guard.len() as u64;
        let enabled = guard.iter().filter(|t| t.enabled).count() as u64;
        Ok(TargetsSummary {
            total,
            enabled,
            disabled: total - enabled,
        })
    }

    async fn set_enabled(&self, _org: OrgId, ids: &[Uuid], enabled: bool) -> Result<Vec<Uuid>> {
        let mut guard = self.targets.lock();
        let now = Utc::now();
        let mut hit = Vec::new();
        for t in guard.iter_mut() {
            if ids.contains(&t.id) {
                t.enabled = enabled;
                t.updated_at = now;
                hit.push(t.id);
            }
        }
        Ok(hit)
    }

    async fn delete_bulk(&self, _org: OrgId, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let mut guard = self.targets.lock();
        let hit: Vec<Uuid> = guard
            .iter()
            .filter(|t| ids.contains(&t.id))
            .map(|t| t.id)
            .collect();
        guard.retain(|t| !ids.contains(&t.id));
        Ok(hit)
    }

    async fn add_tags(&self, _org: OrgId, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>> {
        let mut guard = self.targets.lock();
        let now = Utc::now();
        let mut hit = Vec::new();
        for t in guard.iter_mut() {
            if ids.contains(&t.id) {
                for tag in tags {
                    if !t.tags.iter().any(|x| x == tag) {
                        t.tags.push(tag.clone());
                    }
                }
                t.updated_at = now;
                hit.push(t.id);
            }
        }
        Ok(hit)
    }

    async fn remove_tags(&self, _org: OrgId, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>> {
        let mut guard = self.targets.lock();
        let now = Utc::now();
        let mut hit = Vec::new();
        for t in guard.iter_mut() {
            if ids.contains(&t.id) {
                t.tags.retain(|x| !tags.contains(x));
                t.updated_at = now;
                hit.push(t.id);
            }
        }
        Ok(hit)
    }

    async fn set_group(&self, _org: OrgId, ids: &[Uuid], group: Option<&str>) -> Result<Vec<Uuid>> {
        let mut guard = self.targets.lock();
        let now = Utc::now();
        let mut hit = Vec::new();
        for t in guard.iter_mut() {
            if ids.contains(&t.id) {
                t.group_name = group.map(str::to_owned);
                t.updated_at = now;
                hit.push(t.id);
            }
        }
        Ok(hit)
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl crate::storage::admin::EnabledTargetSource for InMemoryTargetStore {
    /// Single-org test fixture: org is not modelled, so every enabled target
    /// is tagged with the nil-UUID org. The alert fan-out is only ever wired
    /// in production (`ResultFanout::new`); test routers use
    /// `ResultFanout::storage_only`, so this sentinel never reaches a
    /// channel resolution.
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        let org = OrgId(uuid::Uuid::nil());
        Ok(self
            .targets
            .lock()
            .iter()
            .filter(|t| t.enabled)
            .map(|t| (org, t.clone()))
            .collect())
    }
}

#[async_trait]
impl crate::storage::admin::PublicStatusTargetSource for InMemoryTargetStore {
    async fn next_public_status_page(
        &self,
        after: Option<crate::storage::admin::PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>> {
        let org = OrgId(uuid::Uuid::nil());
        // Single-org fixture: every row's org is the nil sentinel, so the
        // `(org, id) > cursor` tuple compare collapses to `id > cursor.id`.
        let cursor_target = after.map(|c| c.target_id);
        let mut hits: Vec<Target> = self
            .targets
            .lock()
            .iter()
            // In-memory fixture has no page model; the writer watches every
            // enabled target (page-membership filtering is a PG-only concern).
            .filter(|t| t.enabled)
            .filter(|t| cursor_target.is_none_or(|cid| t.id > cid))
            .cloned()
            .collect();
        hits.sort_by_key(|t| t.id);
        hits.truncate(limit);
        Ok(hits.into_iter().map(|t| (org, t)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn up_result(
        target: Uuid,
        base: DateTime<Utc>,
        offset_s: i64,
        dur: u32,
        dns: u16,
    ) -> CheckResult {
        CheckResult {
            target_id: target,
            org_id: Uuid::nil(),
            timestamp: base + chrono::Duration::seconds(offset_s),
            status: CheckStatus::Up,
            duration_ms: dur,
            dns_ms: Some(dns),
            connect_ms: Some(10),
            tls_ms: None, // exercises the avgState-skips-NULL path
            ttfb_ms: Some(20),
            response_code: Some(200),
            response_size: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn latency_buckets_aggregate_per_slice_and_skip_null_phases() {
        let store = InMemorySink::new();
        let t = Uuid::from_u128(1);
        let base = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        // Two samples in slice 0 (0s, 30s), one in slice 1 (60s).
        store
            .write_batch(&[
                up_result(t, base, 0, 100, 10),
                up_result(t, base, 30, 200, 30),
                up_result(t, base, 60, 300, 50),
            ])
            .await
            .unwrap();
        let range = TimeRange {
            from: base,
            to: base + chrono::Duration::seconds(120),
        };
        let buckets = store
            .latency_buckets(OrgId(Uuid::nil()), t, ClampedRange::unclamped(range), 60)
            .await
            .unwrap();

        assert_eq!(buckets.len(), 2);
        let b0 = &buckets[0];
        assert_eq!(b0.t, base.timestamp() * 1000, "t is unix-millis");
        assert_eq!(b0.samples, 2);
        assert_eq!(b0.avg, 150); // (100+200)/2
        assert_eq!(b0.dns, 20); // (10+30)/2
        assert_eq!(b0.tls, 0); // every sample NULL → skipped, finalises to 0
        assert_eq!(buckets[1].samples, 1);
        assert_eq!(buckets[1].p50, 300);
    }

    #[tokio::test]
    async fn latency_buckets_isolate_target_and_window() {
        let store = InMemorySink::new();
        let t = Uuid::from_u128(1);
        let base = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        store
            .write_batch(&[
                up_result(t, base, 0, 100, 10),
                up_result(Uuid::from_u128(2), base, 0, 100, 10), // other target
                up_result(t, base, -120, 100, 10),               // before window
            ])
            .await
            .unwrap();
        let range = TimeRange {
            from: base,
            to: base + chrono::Duration::seconds(60),
        };
        let buckets = store
            .latency_buckets(OrgId(Uuid::nil()), t, ClampedRange::unclamped(range), 60)
            .await
            .unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].samples, 1);
    }
}
