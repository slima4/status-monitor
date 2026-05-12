use axum::{Router, routing::get};

use crate::api::handlers;
use crate::app::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::healthz))
        .with_state(state)
}
