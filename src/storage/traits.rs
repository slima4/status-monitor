use async_trait::async_trait;

use crate::domain::{CheckResult, Target};
use crate::error::Result;

#[async_trait]
pub trait ResultSink: Send + Sync {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()>;
}

#[async_trait]
pub trait TargetStore: Send + Sync {
    async fn list_enabled(&self) -> Result<Vec<Target>>;
}
