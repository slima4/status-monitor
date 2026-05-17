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
    SecurityConfig, TransactionalEmailConfig,
};
use status_monitor::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod, OrgId, Target};
use status_monitor::email::{EmailSender, build_email_sender};
use status_monitor::http_client::{HttpClients, build_clients};
use status_monitor::http_outbound::{OutboundHttpClient, build_outbound_client};
use status_monitor::public_status::{NoopPublicSource, PublicSource};
use status_monitor::storage::{
    InMemoryIncidentNarrationStore, InMemoryMaintenanceStore, InMemorySink, InMemoryTargetStore,
    IncidentNarrationStore, MaintenanceStore, PostgresTargetStore, ResultSink, ResultsStore,
};
use status_monitor::worker::{ResultFanout, WorkerPool};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

/// Outbound HTTP client + in-memory email sender shared by every test
/// `AppState` builder. The "memory" provider buffers sends so tests can
/// assert against them.
pub fn build_test_outbound_and_email() -> (OutboundHttpClient, Arc<dyn EmailSender>) {
    let http = build_outbound_client();
    let cfg = TransactionalEmailConfig {
        provider: "memory".into(),
        ..Default::default()
    };
    let sender = build_email_sender(&cfg, &http).expect("memory email sender");
    (http, sender)
}

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
    /// `subdomain_public_routes = true` so non-gating tests aren't blocked by
    /// the public-routes guard.
    pub fn apply(self, cfg: &mut AppConfig) {
        match self {
            TenancyMode::SelfHost => {
                cfg.tenancy.enabled = false;
                cfg.tenancy.path_based_public_routes = true;
                cfg.tenancy.subdomain_public_routes = false;
            }
            TenancyMode::Saas => {
                cfg.tenancy.enabled = true;
                cfg.tenancy.path_based_public_routes = false;
                cfg.tenancy.subdomain_public_routes = true;
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
        build_test_outbound_and_email().0,
        build_test_outbound_and_email().1,
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
        build_test_outbound_and_email().0,
        build_test_outbound_and_email().1,
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
        build_test_outbound_and_email().0,
        build_test_outbound_and_email().1,
    );
    let api = build_router(state.clone(), CancellationToken::new());
    let app = api.merge(status_monitor::web::routes(&state.cfg).with_state(state));
    (app, default_org_id)
}

/// Like [`build_test_app_with_pg`] but the target store is the real
/// `PostgresTargetStore` bound to a **freshly inserted org** (unique slug,
/// `plan_id` defaulting to `free`). Quota counts (`QuotaService`) and the
/// store's atomic count-in-INSERT both read the same `targets` table, and
/// the unique org isolates parallel quota tests from each other. Used by
/// the quota integration suite, which must exercise the production path.
pub async fn build_test_app_with_pg_store(
    pool: PgPool,
    mutate: impl FnOnce(&mut AppConfig),
) -> (Router, OrgId) {
    let mut cfg = AppConfig::load().expect("config");
    mutate(&mut cfg);
    let slug = format!("qt{}", uuid::Uuid::now_v7().simple());
    let slug = &slug[..slug.len().min(30)];
    let (org_uuid,): (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ($1, 'Quota Test') RETURNING id",
    )
    .bind(slug)
    .fetch_one(&pool)
    .await
    .expect("insert quota-test org");
    let default_org_id = OrgId(org_uuid);
    let app = assemble_pg_router(pool, cfg, default_org_id);
    (app, default_org_id)
}

/// SaaS router backed by the **real** `PostgresTargetStore` (no ambient org —
/// every query is scoped by the request's `CurrentOrg`) plus an in-memory
/// results sink. Unlike [`build_test_app_with_pg_store`] this provisions no
/// org: the caller creates as many orgs as the test needs and drives requests
/// with per-org `session_layer`s. This is the harness for cross-tenant IDOR
/// regression — two orgs, one target each, one shared store.
pub async fn build_saas_router_with_pg_targets(pool: PgPool) -> Router {
    let mut cfg = AppConfig::load().expect("config");
    cfg.tenancy.enabled = true;
    cfg.tenancy.path_based_public_routes = false;
    cfg.tenancy.subdomain_public_routes = true;
    // `default_org_id` is the self-host fallback only; SaaS resolves the org
    // per request from the session, so the value here is never read.
    assemble_pg_router(pool, cfg, test_org_id())
}

/// Shared tail of the PG-target-store router builders: real
/// `PostgresTargetStore` + in-memory everything else, wired into the API +
/// web router. Callers own the org/tenancy prelude that precedes this.
fn assemble_pg_router(pool: PgPool, cfg: AppConfig, default_org_id: OrgId) -> Router {
    let target_store = Arc::new(PostgresTargetStore::from_pool(pool.clone(), None));
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
        build_test_outbound_and_email().0,
        build_test_outbound_and_email().1,
    );
    let api = build_router(state.clone(), CancellationToken::new());
    api.merge(status_monitor::web::routes(&state.cfg).with_state(state))
}

/// Layer that stamps the provided `Session` onto every request's extensions.
/// `Session::from_request_parts` reads from the extensions when present, so
/// tests can drive authenticated routes without the real auth backend.
pub fn session_layer(
    session: status_monitor::web::Session,
) -> axum::Extension<status_monitor::web::Session> {
    axum::Extension(session)
}

/// Insert a fresh user with a unique email and return its id. `prefix` only
/// disambiguates the email in shared-DB logs — uniqueness comes from the
/// `Uuid::now_v7()` suffix, so concurrent suites never collide.
pub async fn make_user(pool: &PgPool, prefix: &str) -> status_monitor::domain::UserId {
    let email = format!("{prefix}-{}@test.example", Uuid::now_v7());
    let (id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(pool)
        .await
        .expect("insert user");
    status_monitor::domain::UserId(id)
}

/// `{prefix}-{8 hex}` — the tail of a v4 (pure-random) UUID, so two slugs
/// minted in the same millisecond can't collide (v7's leading bytes are the
/// timestamp). Stays within the 30-char slug limit for the short test prefixes.
pub fn unique_slug(prefix: &str) -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &id[id.len() - 8..])
}

/// Stamp a session onto `router`. `org` becomes `active_org_id` (the
/// `CurrentOrg` extractor verifies `user` is an active member of it);
/// `session_id` sets the session id. Supersedes the per-file `as_member` /
/// `app_with_session` / `with_session` copies.
pub fn with_session(
    router: Router,
    user: status_monitor::domain::UserId,
    org: Option<OrgId>,
    session_id: Option<&str>,
) -> Router {
    router.layer(session_layer(status_monitor::web::Session {
        user: Some(status_monitor::web::User {
            id: user,
            email: format!("u-{}@test.example", user.0),
        }),
        active_org_id: org,
        session_id: session_id.map(str::to_owned),
    }))
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
        build_test_outbound_and_email().0,
        build_test_outbound_and_email().1,
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
        credentials_kek_base64: secrecy::SecretString::from(String::new()),
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
    scheduler_cfg_jittered(refresh_secs, 0)
}

pub fn scheduler_cfg_jittered(refresh_secs: u64, jitter_pct: u8) -> SchedulerConfig {
    SchedulerConfig {
        target_refresh_interval_secs: refresh_secs,
        jitter_pct,
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
        _org: OrgId,
    ) -> Result<
        Arc<status_monitor::domain::PublicStatusPage>,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn component_history(
        &self,
        _org: OrgId,
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
        _org: OrgId,
        _q: status_monitor::public_status::IncidentListQuery,
    ) -> Result<
        status_monitor::api::PageEnvelope<status_monitor::domain::PublicIncident>,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn incident_by_id(
        &self,
        _org: OrgId,
        _id: Uuid,
    ) -> Result<
        status_monitor::domain::PublicIncident,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn maintenance(
        &self,
        _org: OrgId,
    ) -> Result<
        status_monitor::domain::PublicMaintenanceList,
        status_monitor::api::public_error::PublicAppError,
    > {
        Err(status_monitor::api::public_error::PublicAppError::Unavailable)
    }
    async fn incidents_rss(
        &self,
        _org: OrgId,
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

// Per-test throwaway database: `CREATE DATABASE <prefix>_<uuid>`, migrations
// applied by the caller, dropped at the end. The isolation model used by the
// auth / GDPR suites (distinct from the shared-DB `pg_pool_from_env`). Returns
// `None` when `DATABASE_URL` is unset so `#[ignore]` tests no-op cleanly.
pub async fn fresh_test_db(prefix: &str) -> Option<(String, String)> {
    use sqlx::{Connection, Executor};
    let raw = std::env::var("DATABASE_URL").ok()?;
    let mut url = Url::parse(&raw).expect("DATABASE_URL must be a valid URL");
    let test_db = format!("{prefix}_{}", Uuid::now_v7().simple());
    url.set_path("/postgres");
    let mut conn = sqlx::PgConnection::connect(url.as_str())
        .await
        .expect("fresh_test_db: connect to admin DB");
    conn.execute(format!("CREATE DATABASE {test_db}").as_str())
        .await
        .expect("fresh_test_db: CREATE DATABASE");
    let mut new_url = url.clone();
    new_url.set_path(&format!("/{test_db}"));
    Some((new_url.to_string(), test_db))
}

pub async fn drop_test_db(test_db: &str) {
    use sqlx::{Connection, Executor};
    let Ok(raw) = std::env::var("DATABASE_URL") else {
        return;
    };
    let mut url = Url::parse(&raw).expect("DATABASE_URL must be a valid URL");
    url.set_path("/postgres");
    if let Ok(mut conn) = sqlx::PgConnection::connect(url.as_str()).await {
        let _ = conn
            .execute(
                format!(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = '{test_db}' AND pid <> pg_backend_pid()"
                )
                .as_str(),
            )
            .await;
        let _ = conn
            .execute(format!("DROP DATABASE IF EXISTS {test_db}").as_str())
            .await;
    }
}

pub async fn open_test_pool(db_url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .connect(db_url)
        .await
        .expect("open_test_pool: connect to test DB")
}
