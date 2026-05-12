#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use chrono::Utc;
use status_monitor::config::{
    CheckerConfig, CircuitBreakerConfig, DnsConfig, HttpClientConfig, SchedulerConfig,
};
use status_monitor::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod, Target};
use status_monitor::http_client::{HttpClients, build_clients};
use url::Url;
use uuid::Uuid;

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
        cache_size: 1024,
        positive_ttl_secs: 30,
        negative_ttl_secs: 5,
        servers: vec!["127.0.0.1:9".into()],
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
    };
    let checker_cfg = CheckerConfig {
        max_concurrent_checks: 100,
        default_timeout_ms: 5_000,
        connect_timeout_ms: 2_000,
        default_check_interval_secs: 60,
    };
    build_clients(&http_cfg, &checker_cfg, &dns_cfg)
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
