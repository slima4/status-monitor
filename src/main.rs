use std::sync::Arc;

use status_monitor::{
    api::build_router,
    app::AppState,
    config::AppConfig,
    error::{AppError, Result},
    observability,
    storage::{InMemorySink, InMemoryTargetStore},
};
use tokio::net::TcpListener;
use tokio::signal;

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
    let target_store = Arc::new(InMemoryTargetStore::new());
    let results_store = Arc::new(InMemorySink::new());
    let state = AppState::new(cfg, target_store, results_store);
    let router = build_router(state);

    let listener = TcpListener::bind(&api_bind).await.map_err(AppError::Io)?;
    tracing::info!(addr = %api_bind, "api listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(AppError::Io)?;

    let _ = metrics_handle;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
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
