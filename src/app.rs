use std::sync::Arc;

use crate::config::AppConfig;
use crate::storage::{ResultsStore, TargetStore};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub target_store: Arc<dyn TargetStore>,
    pub results_store: Arc<dyn ResultsStore>,
}

impl AppState {
    pub fn new(
        cfg: AppConfig,
        target_store: Arc<dyn TargetStore>,
        results_store: Arc<dyn ResultsStore>,
    ) -> Self {
        Self {
            cfg: Arc::new(cfg),
            target_store,
            results_store,
        }
    }
}
