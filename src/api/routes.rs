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

use crate::api::handlers;
use crate::api::{ApiDoc, idempotency, middleware as api_middleware};
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
        .route("/targets/bulk-action", post(handlers::targets::bulk_action))
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
        .route(
            "/maintenance",
            get(handlers::maintenance::list_maintenance)
                .post(handlers::maintenance::create_maintenance),
        )
        .route(
            "/maintenance/{id}",
            get(handlers::maintenance::get_maintenance)
                .patch(handlers::maintenance::update_maintenance)
                .delete(handlers::maintenance::delete_maintenance),
        )
        .route(
            "/incidents/{id}",
            axum::routing::patch(handlers::incidents::update_incident_narration),
        )
        .route(
            "/incidents/{id}/updates",
            post(handlers::incidents::post_incident_update),
        )
        .route(
            "/orgs",
            get(handlers::orgs::list_my_orgs).post(handlers::orgs::create_org),
        )
        .route("/orgs/check-slug", get(handlers::orgs::check_slug))
        .route(
            "/orgs/{id}",
            get(handlers::orgs::get_org)
                .patch(handlers::orgs::update_org)
                .delete(handlers::orgs::delete_org),
        )
        .route("/orgs/{id}/restore", post(handlers::orgs::restore_org))
        .route("/orgs/{id}/members", get(handlers::orgs::list_org_members))
        .route(
            "/orgs/{id}/members/{user_id}",
            axum::routing::delete(handlers::orgs::remove_org_member),
        )
        .route("/me", get(handlers::me::me))
        .route("/me/sessions", get(handlers::me::list_sessions))
        .route(
            "/me/sessions/{id}",
            axum::routing::delete(handlers::me::revoke_session),
        )
        .route("/me/orgs", get(handlers::orgs::list_my_orgs))
        .route(
            "/me/deleted-orgs",
            get(handlers::orgs::list_my_deleted_orgs),
        )
        .route("/me/active-org", post(handlers::orgs::switch_active_org))
        .route(
            "/me/api-tokens",
            get(handlers::api_tokens::list).post(handlers::api_tokens::create),
        )
        .route(
            "/me/api-tokens/{id}",
            axum::routing::patch(handlers::api_tokens::rename).delete(handlers::api_tokens::revoke),
        )
        .route(
            "/orgs/{id}/invitations",
            get(handlers::invitations::list).post(handlers::invitations::create),
        )
        .route(
            "/orgs/{id}/invitations/{invitation_id}",
            axum::routing::delete(handlers::invitations::revoke),
        )
        .route("/invitations/accept", post(handlers::invitations::accept))
        .route("/invitations/decline", post(handlers::invitations::decline))
        .layer(from_fn_with_state(
            state.clone(),
            crate::web::auth::api_token::middleware,
        ))
        .layer(DefaultBodyLimit::max(SINGLE_BODY_LIMIT))
        .merge(bulk);

    if let Some(layer) = rate_limit_layer(&state.cfg.api.rate_limit, shutdown) {
        v1 = v1.layer(layer);
    }
    if let Some(layer) = cors_layer(&state.cfg.api.cors) {
        v1 = v1.layer(layer);
    }

    let mut auth_routes = Router::new()
        .route("/auth/github/login", get(handlers::auth::github_login))
        .route(
            "/auth/github/callback",
            get(handlers::auth::github_callback),
        )
        .route("/auth/logout", post(handlers::auth::logout))
        .route("/auth/logout-all", post(handlers::auth::logout_all));

    if magic_link_enabled(&state.cfg) {
        auth_routes = auth_routes
            .route(
                "/auth/magic-link/request",
                post(handlers::magic_link::request),
            )
            .route("/auth/magic-link/verify", get(handlers::magic_link::verify));
    }

    let mut root = Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::readyz))
        .merge(auth_routes)
        .nest("/api/v1", v1);

    if public_routes_active(&state.cfg) {
        root = root.nest("/api/public/v1", build_public_router());
    }

    root.merge(SwaggerUi::new("/docs").url("/api/openapi.json", ApiDoc::openapi()))
        .layer(from_fn(api_middleware::cache_control))
        .layer(from_fn(api_middleware::json_charset))
        .layer(from_fn_with_state(
            state.clone(),
            crate::web::auth::csrf::middleware,
        ))
        .layer(tower_cookies::CookieManagerLayer::new())
        .with_state(state)
}

/// Path-based public surface (`/status` HTML + `/api/public/v1/*` JSON on the
/// operator host, scoped to the default org). Defaults to `!tenancy.enabled`:
/// self-host serves its single org here; SaaS forces it off (a startup
/// assertion refuses to boot with this and `tenancy.enabled` both true) so it
/// can't leak the default org to every tenant.
pub fn path_based_public_routes_enabled(cfg: &crate::config::AppConfig) -> bool {
    cfg.tenancy.path_based_public_routes
}

/// Per-org subdomain surface (`*.status.{public_status.base_domain}`, org
/// resolved from the `Host` header). Requires `tenancy.enabled` — a startup
/// assertion refuses to boot if `subdomain_public_routes` is set without it.
pub fn subdomain_public_routes_enabled(cfg: &crate::config::AppConfig) -> bool {
    cfg.tenancy.enabled && cfg.tenancy.subdomain_public_routes
}

/// Whether *any* public surface is mounted. The two surfaces are mutually
/// exclusive in practice (the startup assertions forbid path-based + SaaS,
/// and subdomain needs SaaS), so the shared handlers resolve the org via the
/// host-aware [`crate::web::host::StatusPageOrg`] extractor and only one
/// surface is ever live per deployment.
pub fn public_routes_active(cfg: &crate::config::AppConfig) -> bool {
    path_based_public_routes_enabled(cfg) || subdomain_public_routes_enabled(cfg)
}

/// Whether the `auth.enabled_methods` config includes `"magic_link"`. Wires
/// the request/verify endpoints when true; otherwise the routes are absent
/// and the surface 404s. The schema/templates exist either way — the
/// `magic_link_tokens` table simply stays empty.
pub fn magic_link_enabled(cfg: &crate::config::AppConfig) -> bool {
    cfg.auth.enabled_methods.iter().any(|m| m == "magic_link")
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
        .route("/badge.svg", get(handlers::public::public_badge))
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
