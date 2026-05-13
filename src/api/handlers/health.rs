use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::ApiError;
use crate::app::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    #[schema(example = "ok")]
    pub status: &'static str,
}

#[utoipa::path(
    get,
    path = "/healthz",
    tag = "health",
    summary = "Liveness probe",
    description = "Always returns 200 if the process is alive. Does NOT check dependencies.",
    responses(
        (status = 200, description = "Service is running", body = HealthResponse,
            example = json!({"status": "ok"})),
    ),
)]
pub async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[utoipa::path(
    get,
    path = "/readyz",
    tag = "health",
    summary = "Readiness probe",
    description = "Returns 200 only if all critical dependencies (Postgres, ClickHouse) are reachable.",
    responses(
        (status = 200, description = "Ready to serve traffic", body = HealthResponse,
            example = json!({"status": "ready"})),
        (status = 503, description = "Dependency unavailable", body = ApiError),
    ),
)]
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
