use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use futures::future::join_all;
use status_monitor::config::{CheckerConfig, DnsConfig, HttpClientConfig};
use status_monitor::domain::{ExpectedStatus, HttpCheck, HttpMethod};
use status_monitor::http_client::{HttpClients, build_clients};
use status_monitor::worker::execute_http_check;
use tokio::runtime::{Builder, Runtime};
use url::Url;
use uuid::Uuid;

struct Fixture {
    client_rt: Runtime,
    _server_rt: Runtime,
    clients: HttpClients,
    check: HttpCheck,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        // Split runtimes so the mock server's accept loop never competes with
        // the client futures we're measuring.
        let server_rt = Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("bench-server")
            .build()
            .unwrap();
        let client_rt = Builder::new_multi_thread()
            .enable_all()
            .worker_threads(client_worker_threads())
            .thread_name("bench-client")
            .build()
            .unwrap();

        let addr = server_rt.block_on(spawn_mock());
        let clients = build_test_clients();
        let url = Url::parse(&format!("http://{addr}/")).unwrap();
        Fixture {
            client_rt,
            _server_rt: server_rt,
            clients,
            check: HttpCheck {
                url,
                method: HttpMethod::Get,
                timeout: Duration::from_secs(5),
                follow_redirects: false,
                max_redirects: 0,
                expected_status: ExpectedStatus::Exact(200),
                expected_body_contains: None,
                headers: HashMap::new(),
                body: None,
                verify_tls: false,
                basic_auth: None,
                bearer_token: None,
            },
        }
    })
}

fn client_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get().saturating_sub(2)).max(2))
        .unwrap_or(4)
}

async fn spawn_mock() -> SocketAddr {
    let app = Router::new().route("/", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn build_test_clients() -> HttpClients {
    let http_cfg = HttpClientConfig {
        pool_max_idle_per_host: 1024,
        pool_idle_timeout_secs: 60,
        tcp_keepalive_secs: 60,
        http2_keep_alive_interval_secs: 30,
        http2_keep_alive_timeout_secs: 10,
        http2_keep_alive_while_idle: true,
        user_agent: "StatusMonitor/bench".into(),
        http2_prior_knowledge: true,
    };
    let checker_cfg = CheckerConfig {
        max_concurrent_checks: 100_000,
        default_timeout_ms: 5_000,
        connect_timeout_ms: 2_000,
        default_check_interval_secs: 60,
    };
    let dns_cfg = DnsConfig {
        cache_size: 1024,
        positive_ttl_secs: 30,
        negative_ttl_secs: 5,
        servers: vec!["1.1.1.1".into()],
    };
    build_clients(&http_cfg, &checker_cfg, &dns_cfg).unwrap()
}

fn bench_single(c: &mut Criterion) {
    let f = fixture();
    let mut group = c.benchmark_group("http_check_single");
    group.throughput(Throughput::Elements(1));
    group.bench_function("overhead", |b| {
        b.to_async(&f.client_rt)
            .iter(|| async { execute_http_check(Uuid::now_v7(), &f.check, &f.clients).await });
    });
    group.finish();
}

fn bench_throughput(c: &mut Criterion) {
    let f = fixture();
    let mut group = c.benchmark_group("http_check_throughput");
    group.sample_size(10);
    for &concurrency in &[100usize, 1_000, 10_000, 50_000] {
        group.throughput(Throughput::Elements(concurrency as u64));
        group.measurement_time(Duration::from_secs(if concurrency >= 10_000 {
            30
        } else {
            10
        }));
        group.bench_function(format!("c_{concurrency}"), |b| {
            b.to_async(&f.client_rt).iter(|| async {
                let futs = (0..concurrency)
                    .map(|_| execute_http_check(Uuid::now_v7(), &f.check, &f.clients));
                join_all(futs).await
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_single, bench_throughput);
criterion_main!(benches);
