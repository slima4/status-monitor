#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use status_monitor::api::build_router;
use status_monitor::app::AppState;
use status_monitor::config::{
    AppConfig, CheckerConfig, CircuitBreakerConfig, DnsConfig, HttpClientConfig, SchedulerConfig,
    SecurityConfig,
};
use status_monitor::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod, OrgId, Target};
use status_monitor::http_client::{HttpClients, build_clients};
use status_monitor::public_status::{NoopPublicSource, PublicSource};
use status_monitor::storage::{
    InMemoryIncidentNarrationStore, InMemoryMaintenanceStore, InMemorySink, InMemoryTargetStore,
    IncidentNarrationStore, MaintenanceStore, ResultSink, ResultsStore,
};
use status_monitor::worker::{ResultFanout, WorkerPool};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

/// Fixed org id used in every `build_test_app*` helper. Tests run with
/// in-memory stores that don't enforce the FK to `organizations`, so the
/// value just needs to be stable. Live-DB integration tests must NOT reuse
/// this id — they provision their own org via `storage::ensure_default_org`
/// so the FK on tenant tables resolves.
pub fn test_org_id() -> OrgId {
    OrgId(Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_0001))
}

/// Self-host (`tenancy.enabled = false`) vs SaaS (`tenancy.enabled = true`)
/// runtime modes. Integration tests parameterise over this with `rstest`
/// so both modes are exercised against the same router/handlers.
#[derive(Clone, Copy, Debug)]
pub enum TenancyMode {
    SelfHost,
    Saas,
}

impl TenancyMode {
    /// Apply the mode to a freshly loaded [`AppConfig`]. SaaS mode also flips
    /// `public_routes_enabled = true` so non-gating tests aren't blocked by
    /// the public-routes guard.
    pub fn apply(self, cfg: &mut AppConfig) {
        match self {
            TenancyMode::SelfHost => {
                cfg.tenancy.enabled = false;
                cfg.tenancy.public_routes_enabled = false;
            }
            TenancyMode::Saas => {
                cfg.tenancy.enabled = true;
                cfg.tenancy.public_routes_enabled = true;
            }
        }
    }
}

/// Builds a test router with an InMemory store backend, applying `mutate` to
/// the loaded config before constructing `AppState`. The cancellation token is
/// freshly created and never fires — background tasks (rate-limit GC) leak
/// until the test binary exits, which is fine for short-lived tests.
pub fn build_test_app(mutate: impl FnOnce(&mut AppConfig)) -> Router {
    build_test_app_inner(mutate, false)
}

/// Like `build_test_app` but also returns the in-memory narration store so
/// tests can seed incidents directly. Maintenance routes still go through HTTP.
pub fn build_test_app_with_seedable_incidents(
    mutate: impl FnOnce(&mut AppConfig),
) -> (Router, Arc<InMemoryIncidentNarrationStore>) {
    let mut cfg = AppConfig::load().expect("config");
    mutate(&mut cfg);
    let target_store = Arc::new(InMemoryTargetStore::new());
    let sink = Arc::new(InMemorySink::new());
    let results_store: Arc<dyn ResultsStore> = sink.clone();
    let result_sink: Arc<dyn ResultSink> = sink;
    let http_clients = Arc::new(test_client());
    let (tx, _rx) = mpsc::channel(1024);
    let pool = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks.max(1),
        (*http_clients).clone(),
        cfg.circuit_breaker,
        ResultFanout::storage_only(tx),
    ));
    let public_source = Arc::new(NoopPublicSource::default());
    let maintenance_store: Arc<dyn MaintenanceStore> = Arc::new(InMemoryMaintenanceStore::new());
    let narration = Arc::new(InMemoryIncidentNarrationStore::new());
    let incident_narration_store: Arc<dyn IncidentNarrationStore> = narration.clone();
    let state = AppState::new(
        cfg,
        None,
        target_store,
        results_store,
        result_sink,
        http_clients,
        pool,
        public_source,
        maintenance_store,
        incident_narration_store,
        test_org_id(),
    );
    (build_router(state, CancellationToken::new()), narration)
}

/// Like `build_test_app` but accepts a custom `PublicSource` so contract tests
/// can drive the public surface deterministically without Postgres/ClickHouse.
pub fn build_test_app_with_public_source(
    mutate: impl FnOnce(&mut AppConfig),
    public_source: Arc<dyn PublicSource>,
) -> Router {
    build_test_app_with_public_source_inner(mutate, public_source, false)
}

/// Same as [`build_test_app_with_public_source`] but additionally merges
/// `web::routes()` so the HTML `/status` page is reachable.
pub fn build_test_app_with_web_and_public_source(
    mutate: impl FnOnce(&mut AppConfig),
    public_source: Arc<dyn PublicSource>,
) -> Router {
    build_test_app_with_public_source_inner(mutate, public_source, true)
}

fn build_test_app_with_public_source_inner(
    mutate: impl FnOnce(&mut AppConfig),
    public_source: Arc<dyn PublicSource>,
    with_web: bool,
) -> Router {
    let mut cfg = AppConfig::load().expect("config");
    mutate(&mut cfg);
    let target_store = Arc::new(InMemoryTargetStore::new());
    let sink = Arc::new(InMemorySink::new());
    let results_store: Arc<dyn ResultsStore> = sink.clone();
    let result_sink: Arc<dyn ResultSink> = sink;
    let http_clients = Arc::new(test_client());
    let (tx, _rx) = mpsc::channel(1024);
    let pool = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks.max(1),
        (*http_clients).clone(),
        cfg.circuit_breaker,
        ResultFanout::storage_only(tx),
    ));
    let maintenance_store: Arc<dyn MaintenanceStore> = Arc::new(InMemoryMaintenanceStore::new());
    let incident_narration_store: Arc<dyn IncidentNarrationStore> =
        Arc::new(InMemoryIncidentNarrationStore::new());
    let state = AppState::new(
        cfg,
        None,
        target_store,
        results_store,
        result_sink,
        http_clients,
        pool,
        public_source,
        maintenance_store,
        incident_narration_store,
        test_org_id(),
    );
    let api = build_router(state.clone(), CancellationToken::new());
    if with_web {
        api.merge(status_monitor::web::routes(&state.cfg).with_state(state))
    } else {
        api
    }
}

/// Same as [`build_test_app`] but additionally merges `web::routes()` so the
/// HTML UI is reachable. Mirrors the composition in `src/main.rs`.
pub fn build_test_app_with_web(mutate: impl FnOnce(&mut AppConfig)) -> Router {
    build_test_app_inner(mutate, true)
}

fn build_test_app_inner(mutate: impl FnOnce(&mut AppConfig), with_web: bool) -> Router {
    let state = build_test_app_state(mutate);
    let api = build_router(state.clone(), CancellationToken::new());
    if with_web {
        api.merge(status_monitor::web::routes(&state.cfg).with_state(state))
    } else {
        api
    }
}

/// Build a router backed by InMemory tenant stores but with a real Postgres
/// pool wired into `AppState.db`. Org-management routes need the pool. The
/// `mutate` hook also enables tenancy and flips any other knobs the caller
/// wants. Returns the router plus the `default_org_id` provisioned via
/// `ensure_default_org`.
pub async fn build_test_app_with_pg(
    pool: PgPool,
    mutate: impl FnOnce(&mut AppConfig),
) -> (Router, OrgId) {
    let mut cfg = AppConfig::load().expect("config");
    mutate(&mut cfg);
    let default_org_id = status_monitor::storage::ensure_default_org(&pool, "default")
        .await
        .expect("ensure default org");
    let target_store = Arc::new(InMemoryTargetStore::new());
    let sink = Arc::new(InMemorySink::new());
    let results_store: Arc<dyn ResultsStore> = sink.clone();
    let result_sink: Arc<dyn ResultSink> = sink;
    let http_clients = Arc::new(test_client());
    let (tx, _rx) = mpsc::channel(1024);
    let pool_arc = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks.max(1),
        (*http_clients).clone(),
        cfg.circuit_breaker,
        ResultFanout::storage_only(tx),
    ));
    let public_source = Arc::new(NoopPublicSource::default());
    let maintenance_store: Arc<dyn MaintenanceStore> = Arc::new(InMemoryMaintenanceStore::new());
    let incident_narration_store: Arc<dyn IncidentNarrationStore> =
        Arc::new(InMemoryIncidentNarrationStore::new());
    let state = AppState::new(
        cfg,
        Some(pool),
        target_store,
        results_store,
        result_sink,
        http_clients,
        pool_arc,
        public_source,
        maintenance_store,
        incident_narration_store,
        default_org_id,
    );
    (
        build_router(state, CancellationToken::new()),
        default_org_id,
    )
}

/// Layer that stamps the provided `Session` onto every request's extensions.
/// `Session::from_request_parts` reads from the extensions when present, so
/// tests can drive authenticated routes without the real auth backend.
pub fn session_layer(
    session: status_monitor::web::Session,
) -> axum::Extension<status_monitor::web::Session> {
    axum::Extension(session)
}

/// Shared `AppState` builder used by the router helpers above and by tests
/// that need to exercise an extractor directly without going through HTTP.
/// In-memory stores, no Postgres pool (`db: None`); callers that require a
/// pool must build their own state.
pub fn build_test_app_state(mutate: impl FnOnce(&mut AppConfig)) -> AppState {
    let mut cfg = AppConfig::load().expect("config");
    mutate(&mut cfg);
    let target_store = Arc::new(InMemoryTargetStore::new());
    let sink = Arc::new(InMemorySink::new());
    let results_store: Arc<dyn ResultsStore> = sink.clone();
    let result_sink: Arc<dyn ResultSink> = sink;
    let http_clients = Arc::new(test_client());
    let (tx, _rx) = mpsc::channel(1024);
    let pool = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks.max(1),
        (*http_clients).clone(),
        cfg.circuit_breaker,
        ResultFanout::storage_only(tx),
    ));
    let public_source = Arc::new(NoopPublicSource::default());
    let maintenance_store: Arc<dyn MaintenanceStore> = Arc::new(InMemoryMaintenanceStore::new());
    let incident_narration_store: Arc<dyn IncidentNarrationStore> =
        Arc::new(InMemoryIncidentNarrationStore::new());
    AppState::new(
        cfg,
        None,
        target_store,
        results_store,
        result_sink,
        http_clients,
        pool,
        public_source,
        maintenance_store,
        incident_narration_store,
        test_org_id(),
    )
}

/// Builds a JSON request with `Content-Type: application/json`. Panics on
/// serialization failure — tests pass `serde_json::Value` literals so the
/// only way this fails is a typo in the test fixture.
pub fn json_request(method: &str, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize JSON body"),
        ))
        .expect("build request")
}

/// Decodes the response body as JSON. Panics on non-JSON payloads.
pub async fn body_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20)
        .await
        .expect("collect body");
    serde_json::from_slice(&bytes).expect("valid json")
}

pub async fn spawn_router(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

pub async fn spawn_self_signed_tls_router(router: Router) -> SocketAddr {
    use axum_server::tls_rustls::RustlsConfig;
    use rcgen::generate_simple_self_signed;

    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = generate_simple_self_signed(vec!["localhost".into()]).expect("gen cert");
    let cfg = RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .expect("rustls config");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, cfg)
            .expect("rustls server")
            .serve(router.into_make_service())
            .await
            .expect("serve");
    });

    addr
}

pub fn test_client() -> HttpClients {
    build_clients_with(default_dns()).unwrap()
}

pub fn test_client_with_failing_dns() -> HttpClients {
    build_clients_with(DnsConfig {
        servers: vec!["127.0.0.1:9".into()],
        ..default_dns()
    })
    .unwrap()
}

fn default_dns() -> DnsConfig {
    DnsConfig {
        cache_size: 1024,
        positive_ttl_secs: 30,
        negative_ttl_secs: 5,
        servers: vec!["1.1.1.1".into()],
    }
}

fn build_clients_with(dns_cfg: DnsConfig) -> status_monitor::error::Result<HttpClients> {
    let http_cfg = HttpClientConfig {
        pool_max_idle_per_host: 10,
        pool_idle_timeout_secs: 30,
        tcp_keepalive_secs: 30,
        http2_keep_alive_interval_secs: 30,
        http2_keep_alive_timeout_secs: 10,
        http2_keep_alive_while_idle: true,
        user_agent: "StatusMonitor/test".into(),
        http2_prior_knowledge: false,
    };
    let checker_cfg = CheckerConfig {
        max_concurrent_checks: 100,
        default_timeout_ms: 5_000,
        connect_timeout_ms: 2_000,
        default_check_interval_secs: 60,
    };
    let security_cfg = SecurityConfig {
        allow_private_targets: true,
        credentials_kek_base64: String::new(),
    };
    build_clients(&http_cfg, &checker_cfg, &dns_cfg, &security_cfg)
}

pub fn default_http_check(url: Url, expected: ExpectedStatus) -> HttpCheck {
    HttpCheck {
        url,
        method: HttpMethod::Get,
        timeout: Duration::from_secs(3),
        follow_redirects: false,
        max_redirects: 0,
        expected_status: expected,
        expected_body_contains: None,
        headers: HashMap::new(),
        body: None,
        verify_tls: true,
        basic_auth: None,
        bearer_token: None,
    }
}

pub fn http_target(addr: SocketAddr, path: &str, interval_ms: u64) -> Target {
    let url = Url::parse(&format!("http://{addr}{path}")).unwrap();
    Target {
        id: Uuid::now_v7(),
        name: "test".into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_millis(interval_ms),
        enabled: true,
        tags: vec![],
        alerts: status_monitor::domain::TargetAlerts::default(),
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

pub fn breaker_cfg() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 2,
        success_threshold: 1,
        open_duration_secs: 30,
        half_open_max_calls: 1,
    }
}

pub fn scheduler_cfg(refresh_secs: u64) -> SchedulerConfig {
    SchedulerConfig {
        target_refresh_interval_secs: refresh_secs,
        jitter_pct: 0,
    }
}

/// `PublicSource` whose every method returns `PublicAppError::Unavailable`.
/// Useful for asserting the 503 path on public endpoints without standing up
/// a real aggregator.
pub struct UnavailablePublicSource;

#[async_trait::async_trait]
impl PublicSource for UnavailablePublicSource {
    async fn page(
        &self,
    ) -> Result<
        Arc<status_monitor::domain::PublicStatusPage>,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn component_history(
        &self,
        _id: Uuid,
        _days: u32,
    ) -> Result<
        status_monitor::domain::ComponentHistoryResponse,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn list_incidents(
        &self,
        _q: status_monitor::public_status::IncidentListQuery,
    ) -> Result<
        status_monitor::api::PageEnvelope<status_monitor::domain::PublicIncident>,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn incident_by_id(
        &self,
        _id: Uuid,
    ) -> Result<
        status_monitor::domain::PublicIncident,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn maintenance(
        &self,
    ) -> Result<
        status_monitor::domain::PublicMaintenanceList,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn incidents_rss(
        &self,
        _base_url: &str,
    ) -> Result<String, status_monitor::api::public_error::PublicAppError> {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
}

// ── Live-store helpers (Postgres + ClickHouse) ──────────────────────────────
//
// Both return `None` when the corresponding env var is unset, so callers can
// gate `#[ignore]` integration tests cleanly:
//
//     let Some(pool) = common::pg_pool_from_env().await else { return };
//
// Migrations run **at most once per test binary**, guarded by an async
// `Mutex<bool>` — two `#[tokio::test]` cases in the same binary call these
// helpers concurrently, and the ClickHouse migration 002 drops + recreates
// `check_results` non-idempotently, so an unguarded second call races with
// the first's `CREATE MATERIALIZED VIEW … FROM check_results` and fails with
// `UNKNOWN_TABLE`. Tests share the dev database; use fresh UUIDs per test to
// avoid cross-test interference.

static PG_MIGRATED: Mutex<bool> = Mutex::const_new(false);
static CH_MIGRATED: Mutex<bool> = Mutex::const_new(false);

pub async fn pg_pool_from_env() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("connect to postgres");
    let mut guard = PG_MIGRATED.lock().await;
    if !*guard {
        sqlx::migrate!("./migrations/postgres")
            .run(&pool)
            .await
            .expect("run pg migrations");
        *guard = true;
    }
    Some(pool)
}

pub async fn ch_client_from_env() -> Option<clickhouse::Client> {
    let url = std::env::var("CLICKHOUSE_URL").ok()?;
    let user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "monitor".into());
    let password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "monitor".into());
    let database = std::env::var("CLICKHOUSE_DATABASE").unwrap_or_else(|_| "monitor".into());
    let client = clickhouse::Client::default()
        .with_url(&url)
        .with_database(&database)
        .with_user(&user)
        .with_password(&password);
    let mut guard = CH_MIGRATED.lock().await;
    if !*guard {
        status_monitor::storage::migrate(&client)
            .await
            .expect("run ch migrations");
        *guard = true;
    }
    Some(client)
}
