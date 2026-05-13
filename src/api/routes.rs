use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{get, post};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::PeerIpKeyExtractor;
use tower_http::cors::{AllowOrigin, CorsLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::api::{ApiDoc, idempotency, middleware as api_middleware};
use crate::api::handlers;
use crate::app::AppState;
use crate::config::{CorsConfig, RateLimitConfig};

type RateLimitLayer = GovernorLayer<PeerIpKeyExtractor, governor::middleware::NoOpMiddleware, Body>;

const SINGLE_BODY_LIMIT: usize = 64 * 1024;
const BULK_BODY_LIMIT: usize = 8 * 1024 * 1024;
const RATE_LIMIT_GC_INTERVAL: Duration = Duration::from_secs(60);

pub fn build_router(state: AppState, shutdown: CancellationToken) -> Router {
    idempotency::spawn_pruner(state.idempotency.clone(), shutdown.clone());

    let bulk = Router::new()
        .route("/targets/bulk", post(handlers::targets::bulk_create))
        .route(
            "/targets/bulk-action",
            post(handlers::targets::bulk_action),
        )
        .layer(from_fn_with_state(
            state.idempotency.clone(),
            idempotency::middleware,
        ))
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
        .route("/targets/test", post(handlers::targets::test_check))
        .route(
            "/targets/{id}/check-now",
            post(handlers::targets::check_now),
        )
        .route(
            "/targets/{id}/results",
            get(handlers::results::list_results),
        )
        .route("/targets/{id}/uptime", get(handlers::results::uptime))
        .route(
            "/targets/{id}/incidents",
            get(handlers::results::list_incidents),
        )
        .route("/tags", get(handlers::tags::list_tags))
        .route(
            "/dashboard/summary",
            get(handlers::dashboard::dashboard_summary),
        )
        .layer(DefaultBodyLimit::max(SINGLE_BODY_LIMIT))
        .merge(bulk);

    if let Some(layer) = rate_limit_layer(&state.cfg.api.rate_limit, shutdown) {
        v1 = v1.layer(layer);
    }
    if let Some(layer) = cors_layer(&state.cfg.api.cors) {
        v1 = v1.layer(layer);
    }

    let public_v1 = build_public_router();

    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::readyz))
        .nest("/api/v1", v1)
        .nest("/api/public/v1", public_v1)
        .merge(SwaggerUi::new("/docs").url("/api/openapi.json", ApiDoc::openapi()))
        .layer(from_fn(api_middleware::cache_control))
        .layer(from_fn(api_middleware::json_charset))
        .with_state(state)
}

/// Builds the public, unauthenticated `/api/public/v1/*` router. Lives in its
/// own function and is composed via `nest` so future operator-side
/// middlewares (auth, idempotency, etc.) added to the operator router cannot
/// accidentally cover the public surface.
fn build_public_router() -> Router<AppState> {
    Router::new()
        .route("/status", get(handlers::public::public_status))
        .route(
            "/components/{id}/history",
            get(handlers::public::component_history),
        )
        .route("/incidents", get(handlers::public::public_incidents))
        .route("/incidents/{id}", get(handlers::public::public_incident))
        .route(
            "/incidents.rss",
            get(handlers::public::public_incidents_rss),
        )
        .route("/maintenance", get(handlers::public::public_maintenance))
        .layer(from_fn(api_middleware::public_cache_control))
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

/// Builds a CORS layer when `api.cors.enabled = true`. Wildcard origins are
/// only honored via `allow_any_origin`; literal `"*"` inside `allowed_origins`
/// fails the process at startup so misconfiguration cannot silently open the
/// API to any browser.
fn cors_layer(cfg: &CorsConfig) -> Option<CorsLayer> {
    if !cfg.enabled {
        return None;
    }
    let origin = if cfg.allow_any_origin {
        assert!(
            cfg.allowed_origins.is_empty(),
            "api.cors.allow_any_origin = true is mutually exclusive with allowed_origins"
        );
        AllowOrigin::any()
    } else {
        assert!(
            !cfg.allowed_origins.is_empty(),
            "api.cors.enabled = true requires allowed_origins or allow_any_origin"
        );
        let parsed: Vec<HeaderValue> = cfg
            .allowed_origins
            .iter()
            .map(|o| {
                assert!(
                    !o.contains('*'),
                    "api.cors.allowed_origins entry '{o}' contains '*'; set allow_any_origin = true instead"
                );
                HeaderValue::from_str(o).expect("invalid api.cors.allowed_origins entry")
            })
            .collect();
        AllowOrigin::list(parsed)
    };
    let methods: Vec<Method> = cfg
        .allowed_methods
        .iter()
        .map(|m| {
            m.parse::<Method>()
                .expect("invalid api.cors.allowed_methods entry")
        })
        .collect();
    Some(
        CorsLayer::new()
            .allow_origin(origin)
            .allow_methods(methods)
            .allow_headers([header::CONTENT_TYPE]),
    )
}
