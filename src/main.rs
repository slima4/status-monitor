use std::sync::Arc;
use std::time::Duration;

use status_monitor::{
    api::build_router,
    app::AppState,
    config::AppConfig,
    error::{AppError, Result},
    http_client::client::build_clients,
    observability,
    pipeline::{BatcherConfig, ResultBatcher},
    scheduler::{Scheduler, TargetRegistry},
    storage::{InMemorySink, InMemoryTargetStore, ResultSink, ResultsStore, TargetStore},
    worker::WorkerPool,
};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = AppConfig::load()?;
    observability::tracing::init(&cfg.observability);

    let metrics_handle = if cfg.observability.metrics_enabled {
        Some(observability::metrics::init(&cfg.server.metrics_bind)?)
    } else {
        None
    };

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        api_bind = %cfg.server.api_bind,
        metrics_bind = %cfg.server.metrics_bind,
        "starting status-monitor"
    );

    let api_bind = cfg.server.api_bind.clone();
    let target_store: Arc<dyn TargetStore> = Arc::new(InMemoryTargetStore::new());
    let in_memory_sink = Arc::new(InMemorySink::new());
    let result_sink: Arc<dyn ResultSink> = in_memory_sink.clone();
    let results_store: Arc<dyn ResultsStore> = in_memory_sink;

    let http_clients = build_clients(&cfg.http_client, &cfg.checker, &cfg.dns)?;

    let (result_tx, result_rx) = mpsc::channel(cfg.storage.clickhouse.buffer_size.max(1024));
    let pool = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks,
        http_clients,
        cfg.circuit_breaker,
        result_tx.clone(),
    ));
    let registry = Arc::new(TargetRegistry::new(target_store.clone()));
    let scheduler = Arc::new(Scheduler::new(
        registry.clone(),
        pool.clone(),
        cfg.scheduler.clone(),
    ));

    let batcher_cfg = BatcherConfig {
        batch_size: cfg.storage.clickhouse.batch_size.max(1),
        batch_timeout: Duration::from_millis(cfg.storage.clickhouse.batch_timeout_ms),
    };
    let batcher = ResultBatcher::new(result_rx, result_sink, batcher_cfg);

    let root = CancellationToken::new();
    let scheduler_handle: JoinHandle<()> = {
        let scheduler = scheduler.clone();
        let token = root.clone();
        tokio::spawn(async move {
            if let Err(err) = scheduler.run(token).await {
                tracing::error!(?err, "scheduler exited with error");
            }
        })
    };
    let batcher_handle: JoinHandle<()> = {
        let token = root.clone();
        tokio::spawn(async move { batcher.run(token).await })
    };
    // Floor at 100ms to keep a misconfigured 0 / sub-tick value from spinning.
    let sample_interval =
        Duration::from_millis(cfg.observability.gauge_sample_interval_ms.max(100));
    let sampler_handle: JoinHandle<()> = status_monitor::observability::sampler::spawn(
        pool.clone(),
        registry.clone(),
        &result_tx,
        sample_interval,
        root.clone(),
    );
    drop(result_tx);

    let state = AppState::new(cfg, target_store, results_store);
    let router = build_router(state);

    let listener = TcpListener::bind(&api_bind).await.map_err(AppError::Io)?;
    tracing::info!(addr = %api_bind, "api listening");

    let signal_token = root.clone();
    let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
        wait_for_signal().await;
        signal_token.cancel();
    });

    if let Err(err) = serve.await {
        tracing::error!(?err, "api server error");
        root.cancel();
    }

    tracing::info!("draining background tasks");
    let drain = async {
        let _ = tokio::join!(scheduler_handle, batcher_handle, sampler_handle);
    };
    if timeout(SHUTDOWN_DEADLINE, drain).await.is_err() {
        tracing::warn!(
            deadline_secs = SHUTDOWN_DEADLINE.as_secs(),
            "shutdown deadline exceeded, dropping in-flight work"
        );
    }

    let _ = metrics_handle;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn wait_for_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}
