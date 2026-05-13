use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::domain::{CheckResult, NewTarget, Target, TargetUpdate};
use crate::error::Result;
use crate::storage::traits::{
    ResultSink, ResultsStore, TargetFilter, TargetStore, TimeRange, UptimeStats,
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

#[async_trait]
impl ResultsStore for InMemorySink {
    async fn list_results(
        &self,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
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
        if out.len() > limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn uptime(&self, target_id: Uuid, range: TimeRange) -> Result<UptimeStats> {
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
            created_at: now,
            updated_at: now,
        }
    }
}

#[async_trait]
impl TargetStore for InMemoryTargetStore {
    async fn list(&self, filter: TargetFilter) -> Result<Vec<Target>> {
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

    async fn list_enabled(&self) -> Result<Vec<Target>> {
        Ok(self
            .targets
            .lock()
            .iter()
            .filter(|t| t.enabled)
            .cloned()
            .collect())
    }

    async fn get(&self, id: Uuid) -> Result<Option<Target>> {
        Ok(self.targets.lock().iter().find(|t| t.id == id).cloned())
    }

    async fn create(&self, new: NewTarget) -> Result<Target> {
        let target = Self::materialize(new);
        self.targets.lock().push(target.clone());
        Ok(target)
    }

    async fn update(&self, id: Uuid, update: TargetUpdate) -> Result<Option<Target>> {
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
        t.updated_at = Utc::now();
        Ok(Some(t.clone()))
    }

    async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut guard = self.targets.lock();
        let before = guard.len();
        guard.retain(|t| t.id != id);
        Ok(guard.len() != before)
    }

    async fn bulk_create(&self, items: Vec<NewTarget>) -> Result<Vec<Target>> {
        let mut guard = self.targets.lock();
        let mut created = Vec::with_capacity(items.len());
        for new in items {
            let target = Self::materialize(new);
            guard.push(target.clone());
            created.push(target);
        }
        Ok(created)
    }

    async fn list_updated_since(&self, since: DateTime<Utc>) -> Result<Vec<Target>> {
        Ok(self
            .targets
            .lock()
            .iter()
            .filter(|t| t.updated_at > since)
            .cloned()
            .collect())
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }
}
