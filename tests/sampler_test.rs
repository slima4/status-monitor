mod common;

use std::sync::Arc;
use std::time::Duration;

use status_monitor::http_client::PoolStats;
use status_monitor::observability::sampler;
use status_monitor::scheduler::TargetRegistry;
use status_monitor::storage::InMemoryTargetStore;
use status_monitor::worker::{ResultFanout, WorkerPool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::common::{breaker_cfg, http_target, test_client};

#[tokio::test]
async fn sampler_runs_and_shuts_down() {
    // Long-interval target so its loop never fires during the test; we only
    // need a non-empty registry to exercise targets_total.
    let addr = "127.0.0.1:1".parse().unwrap();
    let store = Arc::new(InMemoryTargetStore::from_vec(vec![http_target(
        addr, "/", 60_000,
    )]));
    let registry = Arc::new(TargetRegistry::new(store.clone()));
    registry.refresh().await.unwrap();

    let (tx, _rx) = mpsc::channel(128);
    let pool = Arc::new(WorkerPool::new(
        16,
        test_client(),
        breaker_cfg(),
        ResultFanout::storage_only(tx.clone()),
        status_monitor::worker::host_throttle::HostThrottle::permissive(),
    ));

    assert_eq!(pool.max_concurrent(), 16);
    assert_eq!(pool.in_flight(), 0);
    assert_eq!(registry.len(), 1);
    assert_eq!(tx.max_capacity(), 128);

    let shutdown = CancellationToken::new();
    let h = sampler::spawn(
        pool,
        registry,
        PoolStats::new(),
        &tx,
        Duration::from_millis(20),
        shutdown.clone(),
    );
    tokio::time::sleep(Duration::from_millis(80)).await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), h)
        .await
        .expect("sampler did not shut down")
        .unwrap();
}
