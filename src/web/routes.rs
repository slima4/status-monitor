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
/// [`public_routes_active`] is true; otherwise SaaS-mode tenants would all
/// see the default org's status page (per-org routing is a follow-on spec).
pub fn routes(cfg: &AppConfig) -> Router<AppState> {
    let mut r = Router::new()
        .route("/", get(views::dashboard::index))
        .route("/targets", get(views::targets_list::index))
        .route("/targets/new", get(views::targets_form::new_form))
        .route("/targets/{id}", get(views::targets_detail::index))
        .route("/targets/{id}/edit", get(views::targets_form::edit_form))
        .route("/web/targets/list", get(views::targets_list::list_partial))
        .route("/web/partials/dashboard", get(views::dashboard::region));

    if public_routes_active(cfg) {
        r = r.route("/status", get(views::public_status::index)).route(
            "/status/incidents/{id}",
            get(views::public_status::incident),
        );
    }

    r.route("/static/{*path}", get(assets::serve))
        .fallback(error::not_found)
        .layer(CookieManagerLayer::new())
}
