use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::PeerIpKeyExtractor;

use crate::api::handlers;
use crate::app::AppState;
use crate::config::RateLimitConfig;

type RateLimitLayer = GovernorLayer<PeerIpKeyExtractor, governor::middleware::NoOpMiddleware, Body>;

const SINGLE_BODY_LIMIT: usize = 64 * 1024;
const BULK_BODY_LIMIT: usize = 8 * 1024 * 1024;
const RATE_LIMIT_GC_INTERVAL: Duration = Duration::from_secs(60);

pub fn build_router(state: AppState, shutdown: CancellationToken) -> Router {
    let bulk = Router::new()
        .route("/targets/bulk", post(handlers::targets::bulk_create))
        .layer(DefaultBodyLimit::max(BULK_BODY_LIMIT));

    let mut v1 = Router::new()
        .route(
            "/targets",
            get(handlers::targets::list).post(handlers::targets::create),
        )
        .route(
            "/targets/{id}",
            get(handlers::targets::get)
                .patch(handlers::targets::update)
                .delete(handlers::targets::delete),
        )
        .route(
            "/targets/{id}/results",
            get(handlers::results::list_results),
        )
        .route("/targets/{id}/uptime", get(handlers::results::uptime))
        .layer(DefaultBodyLimit::max(SINGLE_BODY_LIMIT))
        .merge(bulk);

    if let Some(layer) = rate_limit_layer(&state.cfg.api.rate_limit, shutdown) {
        v1 = v1.layer(layer);
    }

    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::readyz))
        .nest("/api/v1", v1)
        .with_state(state)
}

/// Builds a per-IP token-bucket layer for `/api/v1/*` when enabled. The
/// background task evicts stale per-IP entries so the keyed map stays bounded;
/// it exits when `shutdown` fires, mirroring the sampler/batcher pattern.
fn rate_limit_layer(cfg: &RateLimitConfig, shutdown: CancellationToken) -> Option<RateLimitLayer> {
    if !cfg.enabled {
        return None;
    }
    let conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(u64::from(cfg.per_second))
            .burst_size(cfg.burst)
            .finish()
            .expect("invalid api.rate_limit configuration"),
    );
    let limiter = conf.limiter().clone();
    tokio::spawn(async move {
        let mut ticker = interval(RATE_LIMIT_GC_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => limiter.retain_recent(),
            }
        }
    });
    Some(GovernorLayer::new(conf))
}
