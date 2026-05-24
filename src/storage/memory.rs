use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::api::types::{StatusBreakdown, TagCount, TargetsSummary};
use crate::domain::{
    CheckResult, CheckStatus, Incident, NewTarget, OrgId, Target, TargetUpdate, coalesce_incidents,
};
use crate::error::Result;
use crate::storage::traits::{
    IncidentListQuery, ResultSink, ResultsStore, TargetFilter, TargetStore, TimeRange, UptimeStats,
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
        range: TimeRange,
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

    async fn uptime(&self, _org: OrgId, target_id: Uuid, range: TimeRange) -> Result<UptimeStats> {
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
        let mut incidents = coalesce_for_target(&self.snapshot(), target_id, query.range);
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

    async fn last_n_summary(&self, _org: OrgId, range: TimeRange) -> Result<(u64, u64, u64)> {
        let guard = self.results.lock();
        let mut total = 0u64;
        let mut up = 0u64;
        let mut by_target: std::collections::HashMap<Uuid, Vec<&CheckResult>> =
            std::collections::HashMap::new();
        for r in guard.iter() {
            if r.timestamp < range.from || r.timestamp >= range.to {
                continue;
            }
            total += 1;
            if r.status == CheckStatus::Up {
                up += 1;
            }
            by_target.entry(r.target_id).or_default().push(r);
        }
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
        Ok((total, up, incidents))
    }
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

    fn materialize(new: NewTarget) -> Target {
        let now = Utc::now();
        Target {
            id: Uuid::now_v7(),
            name: new.name,
            check: new.check,
            interval: new.interval,
            enabled: new.enabled,
            tags: new.tags,
            alerts: new.alerts,
            public_status: new.public_status,
            public_name: new.public_name,
            public_description: new.public_description,
            public_group: new.public_group,
            public_sort_order: new.public_sort_order,
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
        let collected: Vec<Target> = guard
            .iter()
            .filter(|t| filter.enabled.map(|e| t.enabled == e).unwrap_or(true))
            .filter(|t| match &filter.tag {
                Some(tag) => t.tags.iter().any(|x| x == tag),
                None => true,
            })
            .skip(filter.offset)
            .take(limit)
            .cloned()
            .collect();
        Ok(collected)
    }

    async fn get(&self, _org: OrgId, id: Uuid) -> Result<Option<Target>> {
        Ok(self.targets.lock().iter().find(|t| t.id == id).cloned())
    }

    async fn create(&self, _org: OrgId, new: NewTarget, max_targets: i64) -> Result<Target> {
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
        let target = Self::materialize(new);
        guard.push(target.clone());
        Ok(target)
    }

    async fn update(&self, _org: OrgId, id: Uuid, update: TargetUpdate) -> Result<Option<Target>> {
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
        if let Some(v) = update.public_status {
            t.public_status = v;
        }
        // Option<Option<String>>: outer Some = field present (inner None
        // clears); outer None = omitted, leave the stored value unchanged.
        if let Some(v) = update.public_name {
            t.public_name = v;
        }
        if let Some(v) = update.public_description {
            t.public_description = v;
        }
        if let Some(v) = update.public_group {
            t.public_group = v;
        }
        if let Some(v) = update.public_sort_order {
            t.public_sort_order = v;
        }
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
            let target = Self::materialize(new);
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
            .filter(|t| t.enabled && t.public_status)
            .filter(|t| cursor_target.is_none_or(|cid| t.id > cid))
            .cloned()
            .collect();
        hits.sort_by_key(|t| t.id);
        hits.truncate(limit);
        Ok(hits.into_iter().map(|t| (org, t)).collect())
    }
}
