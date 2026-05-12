use std::sync::Arc;

use crate::config::AppConfig;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
}

impl AppState {
    pub fn new(cfg: AppConfig) -> Self {
        Self { cfg: Arc::new(cfg) }
    }
}
