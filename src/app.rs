use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache;
use sqlx::PgPool;

use crate::api::IdempotencyCache;
use crate::api::types::DashboardSummary;
use crate::auth::session::{LastUsedDebounce, build_debounce_cache};
use crate::config::AppConfig;
use crate::domain::OrgId;
use crate::email::EmailSender;
use crate::http_client::HttpClients;
use crate::http_outbound::OutboundHttpClient;
use crate::public_status::PublicSource;
use crate::storage::{
    IncidentNarrationStore, MaintenanceStore, ResultSink, ResultsStore, TargetStore,
};
use crate::worker::WorkerPool;

/// Per-org dashboard summary snapshot. Cached for 5 seconds to absorb the
/// operator-dashboard polling cadence. Keyed by `OrgId` so a SaaS tenant's
/// dashboard never reads another tenant's last build.
pub type DashboardCache = Cache<OrgId, Arc<DashboardSummary>>;

/// Builder for the 5-second per-org dashboard cache. The moka `sync::Cache`
/// is cheap to clone (everything inside is `Arc`), so it lives in `AppState`
/// directly rather than behind another `Arc`.
fn build_dashboard_cache() -> DashboardCache {
    Cache::builder()
        .time_to_live(Duration::from_secs(5))
        // 1024 distinct orgs holding a ~few-KB summary is bounded enough that
        // a runaway cache won't eat the heap. Far above any realistic
        // active-org-set in one process.
        .max_capacity(1024)
        .build()
}

/// Runtime handles required by API handlers — the storage layer plus enough
/// scheduler/worker plumbing to support `test`, `check-now`, and the dashboard.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    /// Direct Postgres handle. Required by the `CurrentOrg` extractor (and
    /// future auth helpers) which must read `organizations` / `memberships`
    /// *outside* the tenant-scoped repositories. Org-scoped data access still
    /// goes through the repositories on this state.
    ///
    /// `None` is permitted only for in-memory test fixtures that always run
    /// with `tenancy.enabled = false`; the extractor short-circuits before it
    /// would dereference the pool. Any SaaS-mode code path that observes
    /// `None` here returns an internal error.
    pub db: Option<PgPool>,
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
    /// Org id used in self-host mode and as the implicit org for every write
    /// path until the repository pattern threads `OrgId` through call sites in
    /// Phase 2. Provisioned at startup by `ensure_default_org`.
    pub default_org_id: OrgId,
    /// Debounce cache for `sessions.last_used_at` writes — see
    /// `auth::session::touch_last_used_debounced`.
    pub session_debounce: Arc<LastUsedDebounce>,
    /// Shared outbound HTTPS client used by GitHub OAuth + transactional
    /// email. Not the per-target check client.
    pub outbound_http: OutboundHttpClient,
    /// Transactional email sender (invitations, magic-link). Provider selected
    /// by `email.provider`.
    pub email_sender: Arc<dyn EmailSender>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: AppConfig,
        db: Option<PgPool>,
        target_store: Arc<dyn TargetStore>,
        results_store: Arc<dyn ResultsStore>,
        result_sink: Arc<dyn ResultSink>,
        http_clients: Arc<HttpClients>,
        worker_pool: Arc<WorkerPool>,
        public_source: Arc<dyn PublicSource>,
        maintenance_store: Arc<dyn MaintenanceStore>,
        incident_narration_store: Arc<dyn IncidentNarrationStore>,
        default_org_id: OrgId,
        outbound_http: OutboundHttpClient,
        email_sender: Arc<dyn EmailSender>,
    ) -> Self {
        Self {
            cfg: Arc::new(cfg),
            db,
            target_store,
            results_store,
            result_sink,
            http_clients,
            worker_pool,
            dashboard_cache: build_dashboard_cache(),
            idempotency: Arc::new(IdempotencyCache::new()),
            public_source,
            maintenance_store,
            incident_narration_store,
            default_org_id,
            session_debounce: Arc::new(build_debounce_cache()),
            outbound_http,
            email_sender,
        }
    }
}
