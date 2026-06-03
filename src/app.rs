use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache;
use sqlx::PgPool;

use crate::api::IdempotencyCache;
use crate::api::types::DashboardSummary;
use crate::auth::api_tokens::{
    ApiTokenLastUsedDebounce, build_debounce_cache as build_api_token_debounce,
};
use crate::auth::session::{LastUsedDebounce, build_debounce_cache};
use crate::config::AppConfig;
use crate::domain::OrgId;
use crate::email::EmailSender;
use crate::http_client::HttpClients;
use crate::http_outbound::OutboundHttpClient;
use crate::public_status::PublicSource;
use crate::quotas::{QuotaService, RateLimitService};
use crate::security::AbuseGuard;
use crate::storage::{
    IncidentNarrationStore, MaintenanceStore, NotificationChannelStore, ResultSink, ResultsStore,
    TargetStore,
};
use crate::worker::WorkerPool;

/// Per-org dashboard summary snapshot. Cached for 5 seconds to absorb the
/// operator-dashboard polling cadence. Keyed by `OrgId` so a SaaS tenant's
/// dashboard never reads another tenant's last build.
pub type DashboardCache = Cache<OrgId, Arc<DashboardSummary>>;

/// Detail-page live snapshot (uptime stats + recent results + last-seen
/// status). Cached for 5 seconds so the polling cadence (60s baseline +
/// overdue/manual refreshes arriving in bursts) AND repeat full-page
/// loads (browser back/forward, multi-tab) collapse to a single CH
/// round-trip per window. Keyed `(OrgId, target_id, range_key)` so a
/// tenant never reads another's snapshot and different range tabs don't
/// share cache. `Arc` keeps clones cheap when the full-page handler
/// pulls fields out for the surrounding chrome.
pub type LiveDataCache =
    Cache<(OrgId, uuid::Uuid, &'static str), Arc<crate::web::views::targets_detail::LiveData>>;

/// Operator-dashboard page snapshot: KPI strip + per-monitor rollup +
/// sparkline buckets for one (org, range) pair. Distinct from the
/// `DashboardSummary` API cache above — that one stores the JSON donut
/// payload at `/dashboard/summary`, this one stores the full V3 HTML
/// page snapshot. 5s TTL absorbs both the htmx range re-swap and the
/// auto-refresh poll. `Arc` keeps the cache-hit path a pointer bump
/// (the snapshot can grow large at 1k+ monitors). Keyed on the static
/// range key so the four tabs don't share entries.
pub type DashboardPageCache =
    Cache<(OrgId, &'static str), Arc<crate::web::views::dashboard::DashboardSnapshot>>;

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

/// Sized for ~10k targets × 4 range presets = 40k slots upper bound.
/// Far below that in practice (only actively-viewed targets land in
/// here), but the ceiling caps memory if a crawler hits every target.
/// Each entry ~5 KB → 40k × 5 KB ≈ 200 MB worst case; a quarter of
/// that in practice. moka evicts on capacity AND on the 5s TTL.
fn build_live_data_cache() -> LiveDataCache {
    Cache::builder()
        .time_to_live(Duration::from_secs(5))
        .max_capacity(40_000)
        .build()
}

/// Per-(org, range_key) dashboard-page snapshot cache. Each entry is
/// heavier than a per-target snapshot (one row per monitor + ~60 spark
/// buckets per monitor), so cap entries lower than `LiveDataCache`:
/// 1024 orgs × 4 ranges = 4096 max. moka evicts on capacity AND on
/// the 5s TTL.
fn build_dashboard_page_cache() -> DashboardPageCache {
    Cache::builder()
        .time_to_live(Duration::from_secs(5))
        .max_capacity(4_096)
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
    /// `None` is permitted only for in-memory test fixtures. Any production
    /// code path that observes `None` here returns an internal error via
    /// `require_db()`.
    pub db: Option<PgPool>,
    pub target_store: Arc<dyn TargetStore>,
    pub results_store: Arc<dyn ResultsStore>,
    pub result_sink: Arc<dyn ResultSink>,
    pub http_clients: Arc<HttpClients>,
    pub worker_pool: Arc<WorkerPool>,
    pub dashboard_cache: DashboardCache,
    pub live_data_cache: LiveDataCache,
    pub dashboard_page_cache: DashboardPageCache,
    pub idempotency: Arc<IdempotencyCache>,
    pub public_source: Arc<dyn PublicSource>,
    pub maintenance_store: Arc<dyn MaintenanceStore>,
    pub notification_channel_store: Arc<dyn NotificationChannelStore>,
    pub status_page_store: Arc<dyn crate::storage::StatusPageStore>,
    /// Per-status-page assets (logo now; background/favicon/css later). Built
    /// from `db` so `AppState::new`'s signature stays unchanged: a Pg store
    /// when tenancy is live, an in-memory one for no-DB fixtures.
    pub page_asset_store: Arc<dyn crate::storage::PageAssetStore>,
    /// Per-monitor share links (`/m/{token}`). Built from `db` so
    /// `AppState::new`'s signature stays unchanged: a Pg store when tenancy is
    /// live, an in-memory one for no-DB fixtures.
    pub monitor_share_store: Arc<dyn crate::storage::MonitorShareStore>,
    /// Resolved-channel cache the [`AlertEngine`] reads from. Held here so
    /// the channel CRUD handlers can `invalidate` on edit/delete and the
    /// engine picks up the change on the next dispatch rather than waiting
    /// out the cache TTL.
    pub alert_channel_cache: crate::notifier::engine::AlertChannelCache,
    pub incident_narration_store: Arc<dyn IncidentNarrationStore>,
    /// Operational incident lifecycle (acknowledge/assign/resolve/reopen +
    /// internal timeline). Built from `db` so `AppState::new`'s signature stays
    /// unchanged: a Pg store when tenancy is live, in-memory for no-DB fixtures.
    pub incident_ops_store: Arc<dyn crate::storage::IncidentOpsStore>,
    /// Debounce cache for `sessions.last_used_at` writes — see
    /// `auth::session::touch_last_used_debounced`.
    pub session_debounce: Arc<LastUsedDebounce>,
    /// Debounce cache for `api_tokens.last_used_at` — same shape as
    /// `session_debounce` so the Bearer middleware can lazily refresh
    /// without N writes-per-second per token.
    pub api_token_debounce: Arc<ApiTokenLastUsedDebounce>,
    /// Shared outbound HTTPS client used by GitHub OAuth + transactional
    /// email. Not the per-target check client.
    pub outbound_http: OutboundHttpClient,
    /// Transactional email sender (invitations, magic-link). Provider selected
    /// by `email.provider`.
    pub email_sender: Arc<dyn EmailSender>,
    /// Plan resolution + resource-quota checks. Built from `cfg` + `db` so
    /// `AppState::new`'s signature (and every caller) stays unchanged.
    pub quotas: Arc<QuotaService>,
    /// Per-org / per-user request rate limiter. The idle-entry janitor is
    /// spawned in `build_router` against the shutdown token.
    pub rate_limits: Arc<RateLimitService>,
    /// Compiled URL-pattern + domain deny-list. Built once from `cfg.abuse`;
    /// `main` validates the patterns/YAML first so this build is total.
    pub abuse: Arc<AbuseGuard>,
    /// Escalation-engine signal channel. `Some` only when paging is enabled;
    /// lifecycle handlers (declare/resolve/reopen) nudge the engine through it.
    pub incident_signal_tx: Option<tokio::sync::mpsc::Sender<crate::escalation::IncidentSignal>>,
}

/// Run unconditionally at boot after config parse. Encodes the per-org
/// public-surface and cookie-scope invariants in code so a misconfig is
/// loud and immediate, not a silent runtime data leak. The two functions it
/// calls are kept separate so cookie-scope can be exercised in isolation by
/// tests.
pub fn assert_per_org_status_config(cfg: &AppConfig) {
    if cfg.tenancy.subdomain_public_routes {
        let bd = cfg.public_status.base_domain.as_str();
        if bd.is_empty() || !bd.contains('.') {
            panic!(
                "public_status.base_domain = {bd:?} is empty or missing a dot; \
                 subdomain routing cannot work safely"
            );
        }
    }
    assert_cookie_scope_safe(cfg);
}

/// Refuses to boot when the MCP OAuth server is enabled but its identity URIs
/// are missing or not HTTPS. Without this the AS would mint tokens whose
/// audience is the empty/wrong resource and the resource server would then
/// reject them — a silently-broken connector. Both URIs are also the OAuth
/// `issuer` / `resource` identifiers, which MUST be absolute HTTPS in
/// production. Loopback HTTP is allowed for local development.
pub fn assert_mcp_oauth_config(cfg: &AppConfig) {
    if !cfg.mcp.oauth_enabled {
        return;
    }
    let check = |label: &str, raw: &str| {
        let url = url::Url::parse(raw).unwrap_or_else(|_| {
            panic!(
                "{label} must be a valid absolute URL when mcp.oauth_enabled = true (got {raw:?})"
            )
        });
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            panic!(
                "{label} must be https (or http on loopback for dev) when \
                 mcp.oauth_enabled = true (got {raw:?})"
            );
        }
    };
    if cfg.mcp.resource_uri.trim().is_empty() {
        panic!("mcp.oauth_enabled = true requires mcp.resource_uri to be set");
    }
    if cfg.auth.public_base_url.trim().is_empty() {
        panic!(
            "mcp.oauth_enabled = true requires auth.public_base_url (the OAuth issuer) to be set"
        );
    }
    check("mcp.resource_uri", &cfg.mcp.resource_uri);
    check("auth.public_base_url", &cfg.auth.public_base_url);
}

/// Refuses to boot when `auth.session.cookie_domain` overlaps the per-org
/// status subdomain. Without this, a single config edit on the operator
/// host can leak `_sm_session` to every tenant's status page.
pub fn assert_cookie_scope_safe(cfg: &AppConfig) {
    let cookie_domain = cfg.auth.session.cookie_domain.as_str();
    if cookie_domain.is_empty() {
        return;
    }
    if !cfg.tenancy.subdomain_public_routes {
        return;
    }
    let base = cfg.public_status.base_domain.as_str();
    let cd = cookie_domain.trim_start_matches('.');
    if base == cd || base.ends_with(&format!(".{cd}")) {
        panic!(
            "auth.session.cookie_domain={cookie_domain:?} overlaps the \
             status-page wildcard *.{base}. Operator session cookies would \
             leak to every tenant's status page. Either unset cookie_domain, \
             or move the status surface to a different parent zone."
        );
    }
}

impl AppState {
    /// Borrow the Postgres pool, or return an internal error. Centralises
    /// the "tenancy enabled but db is None" cloak so every handler doesn't
    /// rewrite the same anyhow string.
    pub fn require_db(&self) -> crate::error::Result<&PgPool> {
        self.db.as_ref().ok_or_else(|| {
            crate::error::AppError::Other(anyhow::anyhow!(
                "tenancy enabled but AppState.db is None"
            ))
        })
    }

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
        notification_channel_store: Arc<dyn NotificationChannelStore>,
        status_page_store: Arc<dyn crate::storage::StatusPageStore>,
        incident_narration_store: Arc<dyn IncidentNarrationStore>,
        outbound_http: OutboundHttpClient,
        email_sender: Arc<dyn EmailSender>,
        alert_channel_cache: crate::notifier::engine::AlertChannelCache,
        cipher: Option<Arc<crate::security::Cipher>>,
    ) -> Self {
        let quotas = Arc::new(QuotaService::new(&cfg, db.clone()));
        let monitor_share_store: Arc<dyn crate::storage::MonitorShareStore> = match db.clone() {
            Some(pool) => Arc::new(crate::storage::PgMonitorShareStore::new(pool, cipher)),
            None => Arc::new(crate::storage::InMemoryMonitorShareStore::new()),
        };
        let page_asset_store: Arc<dyn crate::storage::PageAssetStore> = match db.clone() {
            Some(pool) => Arc::new(crate::storage::PgPageAssetStore::new(pool)),
            None => Arc::new(crate::storage::InMemoryPageAssetStore::new()),
        };
        let incident_ops_store: Arc<dyn crate::storage::IncidentOpsStore> = match db.clone() {
            Some(pool) => Arc::new(crate::storage::PgIncidentOpsStore::new(pool)),
            None => Arc::new(crate::storage::InMemoryIncidentOpsStore::new()),
        };
        let rate_limits = Arc::new(RateLimitService::new());
        let abuse = Arc::new(AbuseGuard::from_config(&cfg.abuse));
        Self {
            cfg: Arc::new(cfg),
            db,
            target_store,
            results_store,
            result_sink,
            http_clients,
            worker_pool,
            dashboard_cache: build_dashboard_cache(),
            live_data_cache: build_live_data_cache(),
            dashboard_page_cache: build_dashboard_page_cache(),
            idempotency: Arc::new(IdempotencyCache::new()),
            public_source,
            maintenance_store,
            notification_channel_store,
            status_page_store,
            page_asset_store,
            monitor_share_store,
            incident_narration_store,
            incident_ops_store,
            session_debounce: Arc::new(build_debounce_cache()),
            api_token_debounce: Arc::new(build_api_token_debounce()),
            outbound_http,
            email_sender,
            quotas,
            rate_limits,
            abuse,
            alert_channel_cache,
            incident_signal_tx: None,
        }
    }

    /// Attach the escalation-engine signal channel so lifecycle handlers can
    /// page manual incidents. No-op wiring when paging is disabled.
    pub fn with_incident_signals(
        mut self,
        tx: tokio::sync::mpsc::Sender<crate::escalation::IncidentSignal>,
    ) -> Self {
        self.incident_signal_tx = Some(tx);
        self
    }

    /// Nudge the escalation engine that an incident changed. Best-effort and
    /// non-blocking: drops (logged) when paging is disabled, the engine has
    /// shut down, or the channel is saturated — a lifecycle request must never
    /// block on paging throughput.
    pub fn signal_incident(
        &self,
        org: OrgId,
        incident_id: uuid::Uuid,
        reason: crate::domain::NotificationReason,
    ) {
        if let Some(tx) = &self.incident_signal_tx
            && let Err(err) = tx.try_send(crate::escalation::IncidentSignal {
                org,
                incident_id,
                reason,
            })
        {
            tracing::warn!(%org, %incident_id, error = %err, "incident paging signal dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{assert_cookie_scope_safe, assert_mcp_oauth_config, assert_per_org_status_config};
    use crate::config::AppConfig;

    /// Run `f` with the default panic hook muted (so the expected-panic
    /// cases don't spam the log with backtraces) and assert it unwound with
    /// a message containing `expect`. Matching the message stops a test
    /// passing because it tripped a *different* boot assertion than intended.
    fn assert_panics(expect: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(f);
        std::panic::set_hook(prev);
        let payload = outcome.expect_err("expected a boot-refusing panic");
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert!(
            msg.contains(expect),
            "panicked, but on the wrong assertion: got {msg:?}, expected it to contain {expect:?}"
        );
    }

    /// A valid SaaS-subdomain baseline: subdomain routes on, path-based off,
    /// a two-label base domain, host-only cookies. Every field the assertions
    /// read is set explicitly so env/toml overrides can't make the tests
    /// non-deterministic. Each test then flips exactly the field under test
    /// off this safe starting point.
    fn saas_subdomain_cfg() -> AppConfig {
        let mut cfg = AppConfig::load().expect("config");
        cfg.tenancy.subdomain_public_routes = true;
        cfg.tenancy.path_based_public_routes = false;
        cfg.public_status.base_domain = "example.com".into();
        cfg.auth.session.cookie_domain = String::new();
        cfg
    }

    #[test]
    fn valid_saas_subdomain_config_passes() {
        assert_per_org_status_config(&saas_subdomain_cfg());
    }

    #[test]
    fn empty_base_domain_with_subdomain_routes_panics() {
        let mut cfg = saas_subdomain_cfg();
        cfg.public_status.base_domain = String::new();
        assert_panics("empty or missing a dot", move || {
            assert_per_org_status_config(&cfg)
        });
    }

    #[test]
    fn single_label_base_domain_panics() {
        let mut cfg = saas_subdomain_cfg();
        cfg.public_status.base_domain = "local".into();
        assert_panics("empty or missing a dot", move || {
            assert_per_org_status_config(&cfg)
        });
    }

    #[test]
    fn cookie_domain_overlapping_status_wildcard_panics() {
        // `.example.com` is also sent to `*.example.com`, so the operator
        // session would ride along to every tenant's page.
        let mut cfg = saas_subdomain_cfg();
        cfg.public_status.base_domain = "example.com".into();
        cfg.auth.session.cookie_domain = ".example.com".into();
        assert_panics("overlaps the", move || assert_cookie_scope_safe(&cfg));
    }

    #[test]
    fn cookie_domain_equal_to_base_panics() {
        let mut cfg = saas_subdomain_cfg();
        cfg.public_status.base_domain = "example.com".into();
        cfg.auth.session.cookie_domain = "example.com".into();
        assert_panics("overlaps the", move || assert_cookie_scope_safe(&cfg));
    }

    #[test]
    fn host_only_cookie_is_always_safe() {
        // Empty cookie_domain ⇒ browser scopes to the exact host; no overlap
        // is possible even with an otherwise dangerous base domain.
        let mut cfg = saas_subdomain_cfg();
        cfg.public_status.base_domain = "example.com".into();
        cfg.auth.session.cookie_domain = String::new();
        assert_cookie_scope_safe(&cfg);
    }

    #[test]
    fn disjoint_cookie_domain_is_safe() {
        let mut cfg = saas_subdomain_cfg();
        cfg.public_status.base_domain = "example.com".into();
        cfg.auth.session.cookie_domain = ".other-zone.net".into();
        assert_cookie_scope_safe(&cfg);
    }

    #[test]
    fn cookie_scope_unchecked_when_subdomain_routes_off() {
        // No public subdomains exist, so an overlapping cookie_domain has no
        // cross-tenant surface to leak onto.
        let mut cfg = saas_subdomain_cfg();
        cfg.tenancy.subdomain_public_routes = false;
        cfg.public_status.base_domain = "example.com".into();
        cfg.auth.session.cookie_domain = ".example.com".into();
        assert_cookie_scope_safe(&cfg);
    }

    fn oauth_on_cfg() -> AppConfig {
        let mut cfg = AppConfig::load().expect("config");
        cfg.mcp.oauth_enabled = true;
        cfg.mcp.resource_uri = "https://mcp.example.com/mcp".into();
        cfg.auth.public_base_url = "https://app.example.com".into();
        cfg
    }

    #[test]
    fn oauth_config_valid_https_passes() {
        assert_mcp_oauth_config(&oauth_on_cfg());
    }

    #[test]
    fn oauth_disabled_skips_all_checks() {
        let mut cfg = oauth_on_cfg();
        cfg.mcp.oauth_enabled = false;
        cfg.mcp.resource_uri = String::new();
        cfg.auth.public_base_url = String::new();
        assert_mcp_oauth_config(&cfg);
    }

    #[test]
    fn oauth_on_with_empty_resource_panics() {
        let mut cfg = oauth_on_cfg();
        cfg.mcp.resource_uri = String::new();
        assert_panics("requires mcp.resource_uri", move || {
            assert_mcp_oauth_config(&cfg)
        });
    }

    #[test]
    fn oauth_on_with_non_https_resource_panics() {
        let mut cfg = oauth_on_cfg();
        cfg.mcp.resource_uri = "http://mcp.example.com/mcp".into();
        assert_panics("must be https", move || assert_mcp_oauth_config(&cfg));
    }

    #[test]
    fn oauth_on_with_loopback_http_issuer_passes() {
        let mut cfg = oauth_on_cfg();
        cfg.mcp.resource_uri = "http://localhost:9000/mcp".into();
        cfg.auth.public_base_url = "http://localhost:8080".into();
        assert_mcp_oauth_config(&cfg);
    }
}
