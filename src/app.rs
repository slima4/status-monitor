use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

use crate::api::IdempotencyCache;
use crate::api::types::DashboardSummary;
use crate::config::AppConfig;
use crate::http_client::HttpClients;
use crate::public_status::PublicSource;
use crate::storage::{
    IncidentNarrationStore, MaintenanceStore, ResultSink, ResultsStore, TargetStore,
};
use crate::worker::WorkerPool;

/// Snapshot of the dashboard summary plus the wall-clock instant it was built.
/// Caches for 5 seconds to absorb operator-dashboard polling.
pub type DashboardCache = Arc<Mutex<Option<(Instant, DashboardSummary)>>>;

/// Runtime handles required by API handlers — the storage layer plus enough
/// scheduler/worker plumbing to support `test`, `check-now`, and the dashboard.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub target_store: Arc<dyn TargetStore>,
    pub results_store: Arc<dyn ResultsStore>,
    pub result_sink: Arc<dyn ResultSink>,
    pub http_clients: Arc<HttpClients>,
    pub worker_pool: Arc<WorkerPool>,
    pub dashboard_cache: DashboardCache,
    pub idempotency: Arc<IdempotencyCache>,
    pub public_source: Arc<dyn PublicSource>,
    pub maintenance_store: Arc<dyn MaintenanceStore>,
    pub incident_narration_store: Arc<dyn IncidentNarrationStore>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: AppConfig,
        target_store: Arc<dyn TargetStore>,
        results_store: Arc<dyn ResultsStore>,
        result_sink: Arc<dyn ResultSink>,
        http_clients: Arc<HttpClients>,
        worker_pool: Arc<WorkerPool>,
        public_source: Arc<dyn PublicSource>,
        maintenance_store: Arc<dyn MaintenanceStore>,
        incident_narration_store: Arc<dyn IncidentNarrationStore>,
    ) -> Self {
        Self {
            cfg: Arc::new(cfg),
            target_store,
            results_store,
            result_sink,
            http_clients,
            worker_pool,
            dashboard_cache: Arc::new(Mutex::new(None)),
            idempotency: Arc::new(IdempotencyCache::new()),
            public_source,
            maintenance_store,
            incident_narration_store,
        }
    }
}
