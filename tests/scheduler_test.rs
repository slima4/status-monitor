mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use status_monitor::domain::CheckStatus;
use status_monitor::pipeline::{BatcherConfig, ResultBatcher};
use status_monitor::scheduler::{Scheduler, TargetRegistry};
use status_monitor::storage::{InMemorySink, InMemoryTargetStore};
use status_monitor::worker::circuit_breaker::CIRCUIT_OPEN_REASON;
use status_monitor::worker::{ResultFanout, WorkerPool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::common::{
    breaker_cfg, http_target, scheduler_cfg, scheduler_cfg_jittered, spawn_router, test_client,
};

async fn spawn_counting_mock() -> (std::net::SocketAddr, Arc<AtomicU32>) {
    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();
    let app = Router::new().route(
        "/ping",
        get(move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::Relaxed);
                "pong"
            }
        }),
    );
    (spawn_router(app).await, counter)
}

async fn spawn_always_500() -> std::net::SocketAddr {
    let app = Router::new().route(
        "/",
        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    );
    spawn_router(app).await
}

#[tokio::test]
async fn scheduler_runs_target_periodically() {
    let (addr, counter) = spawn_counting_mock().await;
    let store = Arc::new(InMemoryTargetStore::from_vec(vec![http_target(
        addr, "/ping", 200,
    )]));
    let registry = Arc::new(TargetRegistry::new(store));
    let (tx, mut rx) = mpsc::channel(64);

    let pool = Arc::new(WorkerPool::new(
        50,
        test_client(),
        breaker_cfg(),
        ResultFanout::storage_only(tx),
        status_monitor::worker::host_throttle::HostThrottle::permissive(),
    ));
    let scheduler = Arc::new(Scheduler::new(registry, pool, scheduler_cfg(30)));
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(scheduler.clone().run(shutdown.clone()));

    let mut received = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1200);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(r)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            assert_eq!(r.status, CheckStatus::Up);
            received += 1;
            if received >= 3 {
                break;
            }
        }
    }
    shutdown.cancel();
    handle.await.unwrap().unwrap();

    assert!(received >= 3, "expected ≥3 results, got {received}");
    assert!(counter.load(Ordering::Relaxed) >= 3);
}

// Coverage smoke for the jittered code path: every other scheduler test uses
// `scheduler_cfg(_)`, which hard-codes jitter_pct = 0, so the phase-offset
// branch of `run_target_loop` had zero coverage and could regress to a panic
// or a stall unnoticed. This asserts only what is non-flaky at wall-clock
// resolution: with jitter enabled the target still checks promptly and keeps
// producing results. It deliberately does NOT assert cadence steadiness — the
// fixed-cadence-vs-drift invariant is locked structurally by the `jitter()`
// unit tests (bounded, one-time offset), not by timing assertions here.
#[tokio::test]
async fn scheduler_runs_jittered_target() {
    let (addr, counter) = spawn_counting_mock().await;
    let interval = Duration::from_millis(300);
    let store = Arc::new(InMemoryTargetStore::from_vec(vec![http_target(
        addr,
        "/ping",
        interval.as_millis() as u64,
    )]));
    let registry = Arc::new(TargetRegistry::new(store));
    let (tx, mut rx) = mpsc::channel(64);

    let pool = Arc::new(WorkerPool::new(
        50,
        test_client(),
        breaker_cfg(),
        ResultFanout::storage_only(tx),
        status_monitor::worker::host_throttle::HostThrottle::permissive(),
    ));
    let scheduler = Arc::new(Scheduler::new(
        registry,
        pool,
        scheduler_cfg_jittered(30, 50),
    ));
    let shutdown = CancellationToken::new();
    let start = tokio::time::Instant::now();
    let handle = tokio::spawn(scheduler.clone().run(shutdown.clone()));

    let mut received = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline && received < 3 {
        if let Ok(Some(r)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            assert_eq!(r.status, CheckStatus::Up);
            received += 1;
        }
    }
    let elapsed = start.elapsed();
    shutdown.cancel();
    handle.await.unwrap().unwrap();

    assert!(received >= 3, "expected ≥3 results, got {received}");
    // First check is immediate, the rest one interval apart: 3 results must
    // arrive well inside a window that a stalled/never-firing loop would miss.
    assert!(
        elapsed < interval * 8,
        "3 jittered checks took {elapsed:?}, expected < {:?}",
        interval * 8,
    );
    assert!(
        counter.load(Ordering::Relaxed) >= 3,
        "mock should have served ≥3 pings"
    );
}

#[tokio::test]
async fn scheduler_picks_up_new_targets_on_refresh() {
    let (addr, counter) = spawn_counting_mock().await;
    let store = Arc::new(InMemoryTargetStore::from_vec(vec![]));
    let registry = Arc::new(TargetRegistry::new(store.clone()));
    let (tx, mut rx) = mpsc::channel(64);

    let pool = Arc::new(WorkerPool::new(
        50,
        test_client(),
        breaker_cfg(),
        ResultFanout::storage_only(tx),
        status_monitor::worker::host_throttle::HostThrottle::permissive(),
    ));
    let scheduler = Arc::new(Scheduler::new(registry, pool, scheduler_cfg(1)));
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(scheduler.clone().run(shutdown.clone()));

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(scheduler.active_tasks(), 0);

    store.insert(http_target(addr, "/ping", 200));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut got = false;
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            got = true;
            break;
        }
    }
    shutdown.cancel();
    handle.await.unwrap().unwrap();
    assert!(got, "no result after adding target");
    assert!(counter.load(Ordering::Relaxed) >= 1);
}

#[tokio::test]
async fn shutdown_drains_in_flight_results() {
    let (addr, counter) = spawn_counting_mock().await;
    let store = Arc::new(InMemoryTargetStore::from_vec(vec![http_target(
        addr, "/ping", 100,
    )]));
    let registry = Arc::new(TargetRegistry::new(store));
    let (tx, rx) = mpsc::channel(64);
    let sink = Arc::new(InMemorySink::new());

    let pool = Arc::new(WorkerPool::new(
        50,
        test_client(),
        breaker_cfg(),
        ResultFanout::storage_only(tx.clone()),
        status_monitor::worker::host_throttle::HostThrottle::permissive(),
    ));
    let scheduler = Arc::new(Scheduler::new(registry, pool, scheduler_cfg(30)));
    let batcher = ResultBatcher::new(
        rx,
        sink.clone(),
        BatcherConfig {
            batch_size: 1000,
            batch_timeout: Duration::from_secs(60),
        },
    );
    let shutdown = CancellationToken::new();
    let scheduler_handle = tokio::spawn(scheduler.run(shutdown.clone()));
    let batcher_handle = tokio::spawn(batcher.run(shutdown.clone()));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && counter.load(Ordering::Relaxed) < 1 {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(counter.load(Ordering::Relaxed) >= 1, "no checks ran");
    assert_eq!(sink.len(), 0, "batcher should not have flushed yet");

    shutdown.cancel();
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), scheduler_handle)
        .await
        .expect("scheduler did not exit")
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), batcher_handle)
        .await
        .expect("batcher did not exit")
        .unwrap();

    assert!(
        !sink.is_empty(),
        "shutdown should drain in-flight results, got {}",
        sink.len()
    );
}

#[tokio::test]
async fn worker_pool_breaker_opens_after_failures() {
    let addr = spawn_always_500().await;
    let store = Arc::new(InMemoryTargetStore::from_vec(vec![http_target(
        addr, "/", 100,
    )]));
    let registry = Arc::new(TargetRegistry::new(store));
    let (tx, mut rx) = mpsc::channel(64);

    let pool = Arc::new(WorkerPool::new(
        50,
        test_client(),
        breaker_cfg(),
        ResultFanout::storage_only(tx),
        status_monitor::worker::host_throttle::HostThrottle::permissive(),
    ));
    let scheduler = Arc::new(Scheduler::new(registry, pool.clone(), scheduler_cfg(30)));
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(scheduler.run(shutdown.clone()));

    let mut downs = 0;
    let mut circuit_opens = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && circuit_opens == 0 {
        if let Ok(Some(r)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            match r.status {
                CheckStatus::Down => downs += 1,
                CheckStatus::Error if r.error.as_deref() == Some(CIRCUIT_OPEN_REASON) => {
                    circuit_opens += 1
                }
                _ => {}
            }
        }
    }
    shutdown.cancel();
    handle.await.unwrap().unwrap();

    assert!(downs >= 2, "expected ≥2 downs, got {downs}");
    assert!(circuit_opens >= 1, "breaker never opened");
    assert!(pool.open_breakers() >= 1);
}
