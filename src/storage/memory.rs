use async_trait::async_trait;
use parking_lot::Mutex;

use crate::domain::{CheckResult, Target};
use crate::error::Result;
use crate::storage::traits::{ResultSink, TargetStore};

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
}

#[async_trait]
impl TargetStore for InMemoryTargetStore {
    async fn list_enabled(&self) -> Result<Vec<Target>> {
        Ok(self
            .targets
            .lock()
            .iter()
            .filter(|t| t.enabled)
            .cloned()
            .collect())
    }
}
