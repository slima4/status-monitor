use axum::Router;
use axum::routing::get;
use tower_cookies::CookieManagerLayer;

use crate::app::AppState;
use crate::web::{assets, error, views};

/// Builds the UI router (`Router<AppState>`). Caller is responsible for
/// merging into the main router and applying `with_state`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(views::dashboard::index))
        .route("/targets", get(views::targets_list::index))
        .route("/targets/{id}", get(views::targets_detail::index))
        .route("/web/targets/list", get(views::targets_list::list_partial))
        .route("/static/{*path}", get(assets::serve))
        .fallback(error::not_found)
        .layer(CookieManagerLayer::new())
}
