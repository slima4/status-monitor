use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::get;
use parking_lot::Mutex;
use status_monitor::config::{CheckerConfig, DnsConfig, HttpClientConfig};
use status_monitor::domain::{ExpectedStatus, HttpCheck, HttpMethod};
use status_monitor::http_client::build_clients;
use status_monitor::worker::execute_http_check;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

const LATENCY_BATCH: usize = 1024;

#[derive(Debug)]
struct Args {
    concurrency: usize,
    duration_secs: u64,
    timeout_ms: u64,
}

impl Args {
    fn parse() -> Self {
        let concurrency = env_parse("CONCURRENCY", 50_000usize).max(1);
        let duration_secs = env_parse("DURATION_SECS", 30u64);
        let timeout_ms = env_parse("TIMEOUT_MS", 5_000u64);
        Self {
            concurrency,
            duration_secs,
            timeout_ms,
        }
    }
}

fn env_parse<T: FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    println!("loadtest config: {args:?}");

    let mock_addr = spawn_mock_server().await;
    println!("mock server: http://{mock_addr}");

    let http_cfg = HttpClientConfig {
        pool_max_idle_per_host: 1024,
        pool_idle_timeout_secs: 90,
        tcp_keepalive_secs: 60,
        http2_keep_alive_interval_secs: 30,
        http2_keep_alive_timeout_secs: 10,
        http2_keep_alive_while_idle: true,
        user_agent: "StatusMonitor-LoadTest/1.0".into(),
    };
    let checker_cfg = CheckerConfig {
        max_concurrent_checks: args.concurrency,
        default_timeout_ms: args.timeout_ms,
        connect_timeout_ms: 2_000,
        default_check_interval_secs: 60,
    };
    // No external resolvers — the mock is on loopback; hickory should never be queried.
    let dns_cfg = DnsConfig {
        cache_size: 1024,
        positive_ttl_secs: 300,
        negative_ttl_secs: 60,
        servers: vec![],
    };
    let clients = build_clients(&http_cfg, &checker_cfg, &dns_cfg).expect("build clients");

    let url = Url::parse(&format!("http://{mock_addr}/ok")).expect("parse url");
    let check = Arc::new(HttpCheck {
        url,
        method: HttpMethod::Get,
        timeout: Duration::from_millis(args.timeout_ms),
        follow_redirects: false,
        max_redirects: 0,
        expected_status: ExpectedStatus::Exact(200),
        expected_body_contains: None,
        headers: HashMap::new(),
        body: None,
        verify_tls: true,
        basic_auth: None,
        bearer_token: None,
    });

    let target_id = Uuid::now_v7();
    let total = Arc::new(AtomicU64::new(0));
    let success = Arc::new(AtomicU64::new(0));
    let latencies: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.duration_secs);

    let mut set: JoinSet<()> = JoinSet::new();
    for _ in 0..args.concurrency {
        let clients = clients.clone();
        let check = check.clone();
        let total = total.clone();
        let success = success.clone();
        let latencies = latencies.clone();
        set.spawn(async move {
            let mut local: Vec<u32> = Vec::with_capacity(LATENCY_BATCH);
            while Instant::now() < deadline {
                let r = execute_http_check(target_id, &check, &clients).await;
                total.fetch_add(1, Ordering::Relaxed);
                if matches!(
                    r.status,
                    status_monitor::domain::CheckStatus::Up
                        | status_monitor::domain::CheckStatus::Degraded
                ) {
                    success.fetch_add(1, Ordering::Relaxed);
                }
                local.push(r.duration_ms);
                if local.len() == LATENCY_BATCH {
                    latencies.lock().extend(local.drain(..));
                }
            }
            if !local.is_empty() {
                latencies.lock().extend(local);
            }
        });
    }

    let progress_stop = CancellationToken::new();
    let progress = tokio::spawn({
        let total = total.clone();
        let stop = progress_stop.clone();
        async move {
            let mut last = 0u64;
            let mut last_t = Instant::now();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = sleep(Duration::from_secs(5)) => {}
                }
                let now = total.load(Ordering::Relaxed);
                let dt = last_t.elapsed().as_secs_f64();
                let rps = ((now - last) as f64 / dt) as u64;
                println!("[+{:>4}s] total={now} rps={rps}", start.elapsed().as_secs());
                last = now;
                last_t = Instant::now();
            }
        }
    });

    set.join_all().await;
    let elapsed = start.elapsed().as_secs_f64();
    progress_stop.cancel();
    let _ = progress.await;

    let total = total.load(Ordering::Relaxed);
    let success = success.load(Ordering::Relaxed);
    let mut samples = std::mem::take(&mut *latencies.lock());

    let rps = total as f64 / elapsed;
    let mut p = |q: f64| -> u32 {
        if samples.is_empty() {
            return 0;
        }
        let idx = ((samples.len() as f64) * q).clamp(0.0, (samples.len() - 1) as f64) as usize;
        let (_, nth, _) = samples.select_nth_unstable(idx);
        *nth
    };
    let p50 = p(0.50);
    let p95 = p(0.95);
    let p99 = p(0.99);

    println!("---");
    println!("total checks: {total}");
    println!(
        "success: {success} ({:.2}%)",
        success as f64 * 100.0 / total.max(1) as f64
    );
    println!("errors: {}", total - success);
    println!("elapsed s: {elapsed:.2}");
    println!("rps: {rps:.0}");
    println!("p50 ms: {p50}");
    println!("p95 ms: {p95}");
    println!("p99 ms: {p99}");
}

async fn spawn_mock_server() -> SocketAddr {
    let app = Router::new().route("/ok", get(|| async { "ok" }));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}
