use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use status_monitor::domain::{CheckResult, CheckStatus};
use status_monitor::pipeline::{BatcherConfig, ResultBatcher};
use status_monitor::storage::InMemorySink;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn sample_result() -> CheckResult {
    CheckResult {
        target_id: Uuid::now_v7(),
        timestamp: Utc::now(),
        status: CheckStatus::Up,
        duration_ms: 10,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: None,
        response_code: Some(200),
        response_size: Some(4),
        error: None,
    }
}

#[tokio::test]
async fn flushes_on_size_trigger() {
    let (tx, rx) = mpsc::channel(64);
    let sink = Arc::new(InMemorySink::new());
    let cfg = BatcherConfig {
        batch_size: 3,
        batch_timeout: Duration::from_secs(60),
    };
    let batcher = ResultBatcher::new(rx, sink.clone(), cfg);
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(batcher.run(shutdown.clone()));

    for _ in 0..3 {
        tx.send(sample_result()).await.unwrap();
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline && sink.len() < 3 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(sink.len(), 3);

    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
}

#[tokio::test]
async fn flushes_on_timeout_trigger() {
    let (tx, rx) = mpsc::channel(64);
    let sink = Arc::new(InMemorySink::new());
    let cfg = BatcherConfig {
        batch_size: 100,
        batch_timeout: Duration::from_millis(100),
    };
    let batcher = ResultBatcher::new(rx, sink.clone(), cfg);
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(batcher.run(shutdown.clone()));

    tx.send(sample_result()).await.unwrap();
    tx.send(sample_result()).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline && sink.len() < 2 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(sink.len(), 2);

    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
}

#[tokio::test]
async fn drains_on_shutdown() {
    let (tx, rx) = mpsc::channel(64);
    let sink = Arc::new(InMemorySink::new());
    let cfg = BatcherConfig {
        batch_size: 1000,
        batch_timeout: Duration::from_secs(60),
    };
    let batcher = ResultBatcher::new(rx, sink.clone(), cfg);
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(batcher.run(shutdown.clone()));

    for _ in 0..5 {
        tx.send(sample_result()).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(sink.len(), 0, "no premature flush expected");

    shutdown.cancel();
    handle.await.unwrap();
    assert_eq!(sink.len(), 5, "shutdown should drain pending results");
}

#[tokio::test]
async fn flushes_when_sender_dropped() {
    let (tx, rx) = mpsc::channel(64);
    let sink = Arc::new(InMemorySink::new());
    let cfg = BatcherConfig {
        batch_size: 1000,
        batch_timeout: Duration::from_secs(60),
    };
    let batcher = ResultBatcher::new(rx, sink.clone(), cfg);
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(batcher.run(shutdown.clone()));

    tx.send(sample_result()).await.unwrap();
    tx.send(sample_result()).await.unwrap();
    drop(tx);

    handle.await.unwrap();
    assert_eq!(sink.len(), 2);
}
