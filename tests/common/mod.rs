#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use chrono::Utc;
use status_monitor::api::build_router;
use status_monitor::app::AppState;
use status_monitor::config::{
    AppConfig, CheckerConfig, CircuitBreakerConfig, DnsConfig, HttpClientConfig, SchedulerConfig,
    SecurityConfig,
};
use status_monitor::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod, Target};
use status_monitor::http_client::{HttpClients, build_clients};
use status_monitor::storage::{InMemorySink, InMemoryTargetStore, ResultSink, ResultsStore};
use status_monitor::worker::{ResultFanout, WorkerPool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

/// Builds a test router with an InMemory store backend, applying `mutate` to
/// the loaded config before constructing `AppState`. The cancellation token is
/// freshly created and never fires — background tasks (rate-limit GC) leak
/// until the test binary exits, which is fine for short-lived tests.
pub fn build_test_app(mutate: impl FnOnce(&mut AppConfig)) -> Router {
    build_test_app_inner(mutate, false)
}

/// Same as [`build_test_app`] but additionally merges `web::routes()` so the
/// HTML UI is reachable. Mirrors the composition in `src/main.rs`.
pub fn build_test_app_with_web(mutate: impl FnOnce(&mut AppConfig)) -> Router {
    build_test_app_inner(mutate, true)
}

fn build_test_app_inner(mutate: impl FnOnce(&mut AppConfig), with_web: bool) -> Router {
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
    let state = AppState::new(
        cfg,
        target_store,
        results_store,
        result_sink,
        http_clients,
        pool,
    );
    let api = build_router(state.clone(), CancellationToken::new());
    if with_web {
        api.merge(status_monitor::web::routes().with_state(state))
    } else {
        api
    }
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
