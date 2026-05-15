use axum::Router;
use axum::routing::get;
use tower_cookies::CookieManagerLayer;

use crate::api::public_routes_active;
use crate::app::AppState;
use crate::config::AppConfig;
use crate::web::{assets, error, views};

/// Builds the UI router (`Router<AppState>`). Caller is responsible for
/// merging into the main router and applying `with_state`. The public-status
/// pages (`/status`, `/status/incidents/{id}`) are mounted only when
/// [`public_routes_active`] is true; the org they render is resolved
/// per-request by the host-aware `StatusPageOrg` extractor (subdomain →
/// that tenant; self-host → the default org).
pub fn routes(cfg: &AppConfig) -> Router<AppState> {
    let mut r = Router::new()
        .route("/", get(views::dashboard::index))
        .route("/targets", get(views::targets_list::index))
        .route("/targets/new", get(views::targets_form::new_form))
        .route("/targets/{id}", get(views::targets_detail::index))
        .route("/targets/{id}/edit", get(views::targets_form::edit_form))
        .route("/web/targets/list", get(views::targets_list::list_partial))
        .route("/web/partials/dashboard", get(views::dashboard::region))
        .route("/login", get(views::auth::login))
        .route("/onboarding/org", get(views::auth::onboarding_org))
        .route(
            "/settings/sessions",
            get(views::auth::settings::sessions_page),
        )
        .route(
            "/settings/api-tokens",
            get(views::auth::settings::api_tokens_page),
        )
        .route(
            "/web/partials/settings/sessions",
            get(views::auth::settings::sessions_partial),
        )
        .route(
            "/web/partials/settings/api-tokens",
            get(views::auth::settings::api_tokens_partial),
        );

    if public_routes_active(cfg) {
        r = r
            .route("/status", get(views::public_status::index))
            .route(
                "/status/incidents/{id}",
                get(views::public_status::incident),
            )
            .route(
                views::public_status::LOGO_ROUTE,
                get(views::public_status::logo),
            );
    }

    r.route("/static/{*path}", get(assets::serve))
        .fallback(error::not_found)
        .layer(CookieManagerLayer::new())
}
