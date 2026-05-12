use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::app::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

pub async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    match state.target_store.ping().await {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ready" })),
        Err(err) => {
            tracing::warn!(?err, "readiness probe failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "not_ready",
                }),
            )
        }
    }
}
