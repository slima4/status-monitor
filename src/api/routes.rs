use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

use crate::api::handlers;
use crate::app::AppState;

const SINGLE_BODY_LIMIT: usize = 64 * 1024;
const BULK_BODY_LIMIT: usize = 8 * 1024 * 1024;

pub fn build_router(state: AppState) -> Router {
    let bulk = Router::new()
        .route("/targets/bulk", post(handlers::targets::bulk_create))
        .layer(DefaultBodyLimit::max(BULK_BODY_LIMIT));

    let v1 = Router::new()
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

    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::readyz))
        .nest("/api/v1", v1)
        .with_state(state)
}
