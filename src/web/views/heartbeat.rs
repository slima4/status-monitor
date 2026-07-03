//! Inbound heartbeat ping (`/ping/{token}`). Unauthenticated: holding the
//! token is the proof, same trust model as `/m/{token}` share links. GET and
//! POST both count so curl, cron, and HTTP clients stay one-liners.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
use crate::auth::sha256_hex;

/// Rate-limit key for a token: first half of its SHA-256, so GCRA runs before
/// any database work.
fn token_key(raw: &str) -> u128 {
    let hex = sha256_hex(raw);
    u128::from_str_radix(&hex[..32], 16).unwrap_or(0)
}

pub async fn ping(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !state.heartbeat_runtime.allow_ping(token_key(&token)) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "1")],
            "slow down",
        )
            .into_response();
    }
    // Unknown, revoked, and deleted-org tokens are indistinguishable
    // (anti-enumeration).
    match state.heartbeat_store.record_ping_by_token(&token).await {
        Ok(Some((target_id, at))) => {
            state.heartbeat_runtime.set_anchor(target_id, at);
            tracing::debug!(%target_id, "heartbeat ping accepted");
            (StatusCode::OK, "ok").into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            tracing::error!(error = %err, "heartbeat ping: record failed");
            (StatusCode::SERVICE_UNAVAILABLE, "try again").into_response()
        }
    }
}
