use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, NewTarget, Target, TargetUpdate};
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

#[async_trait]
pub trait TargetStore: Send + Sync {
    async fn list(&self, filter: TargetFilter) -> Result<Vec<Target>>;
    async fn list_enabled(&self) -> Result<Vec<Target>>;
    async fn get(&self, id: Uuid) -> Result<Option<Target>>;
    async fn create(&self, new: NewTarget) -> Result<Target>;
    async fn update(&self, id: Uuid, update: TargetUpdate) -> Result<Option<Target>>;
    async fn delete(&self, id: Uuid) -> Result<bool>;
    async fn bulk_create(&self, items: Vec<NewTarget>) -> Result<Vec<Target>>;
    async fn list_updated_since(&self, since: DateTime<Utc>) -> Result<Vec<Target>>;
    async fn ping(&self) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct UptimeStats {
    pub total: u64,
    pub up: u64,
    pub down: u64,
    pub degraded: u64,
    pub error: u64,
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

#[async_trait]
pub trait ResultsStore: Send + Sync {
    async fn list_results(
        &self,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
    ) -> Result<Vec<CheckResult>>;
    async fn uptime(&self, target_id: Uuid, range: TimeRange) -> Result<UptimeStats>;
}
