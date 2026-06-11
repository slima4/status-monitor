//! Inbound webhook receiver for the central Telegram bot (`/hooks/telegram`).
//!
//! Telegram has no signature scheme, so the only auth is the
//! `X-Telegram-Bot-Api-Secret-Token` header echoing the configured secret,
//! compared in constant time. Every accepted update is answered 200 fast:
//! Telegram retries non-2xx aggressively, so a malformed or unhandled update
//! is still acknowledged.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret;

use crate::app::AppState;
use crate::telegram::{Update, webhook_secret_matches};

const SECRET_HEADER: &str = "x-telegram-bot-api-secret-token";

pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let expected = state.cfg.telegram.webhook_secret.expose_secret();
    let provided = headers
        .get(SECRET_HEADER)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if expected.is_empty() || !webhook_secret_matches(provided, expected) {
        tracing::warn!("telegram webhook rejected: secret token mismatch");
        return StatusCode::FORBIDDEN;
    }

    match serde_json::from_slice::<Update>(&body) {
        Ok(_update) => StatusCode::OK,
        Err(err) => {
            tracing::warn!(?err, "telegram webhook: unparseable update body");
            StatusCode::OK
        }
    }
}
