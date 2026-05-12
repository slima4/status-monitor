use status_monitor::{
    api::build_router,
    app::AppState,
    config::AppConfig,
    error::{AppError, Result},
    observability,
};
use tokio::net::TcpListener;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = AppConfig::load()?;
    observability::tracing::init(&cfg.observability);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        api_bind = %cfg.server.api_bind,
        "starting status-monitor"
    );

    let api_bind = cfg.server.api_bind.clone();
    let state = AppState::new(cfg);
    let router = build_router(state);

    let listener = TcpListener::bind(&api_bind).await.map_err(AppError::Io)?;
    tracing::info!(addr = %api_bind, "api listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(AppError::Io)?;

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
