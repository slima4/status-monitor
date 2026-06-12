//! Resend webhook receiver (`/hooks/resend`): provider lifecycle for email
//! channels. A permanently bounced or spam-complaining address gets every
//! email channel pointed at it disabled, across orgs — the alternative is
//! paging a dead mailbox forever and burning sender reputation. Transient
//! bounces and all other events are acknowledged untouched. Svix signature
//! is the only authentication; a store failure answers 5xx so Svix retries
//! the (idempotent) disable.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::app::AppState;
use crate::domain::ChannelKind;
use crate::email::{mask_email, svix};

pub const HARD_BOUNCE_NOTE: &str = "the email address hard-bounced";
pub const COMPLAINT_NOTE: &str = "the recipient reported our mail as spam";

#[derive(Debug, Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: EventData,
}

#[derive(Debug, Default, Deserialize)]
struct EventData {
    #[serde(default)]
    to: Vec<String>,
    bounce: Option<Bounce>,
}

#[derive(Debug, Deserialize)]
struct Bounce {
    #[serde(rename = "type", default)]
    kind: String,
}

pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let secret = state.cfg.email.resend.webhook_secret.expose_secret();
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    };
    if secret.trim().is_empty()
        || !svix::verify(
            secret,
            h("svix-id"),
            h("svix-timestamp"),
            h("svix-signature"),
            &body,
            Utc::now().timestamp(),
        )
    {
        tracing::warn!("resend webhook rejected: bad signature");
        return StatusCode::FORBIDDEN;
    }
    let event = match serde_json::from_slice::<Event>(&body) {
        Ok(e) => e,
        Err(err) => {
            // Schema drift, not transport trouble — a retry replays the
            // same body, so acknowledge instead of looping.
            tracing::warn!(?err, "resend webhook: unparseable event body");
            return StatusCode::OK;
        }
    };
    let note = match event.kind.as_str() {
        "email.bounced"
            if event
                .data
                .bounce
                .as_ref()
                .is_some_and(|b| b.kind.eq_ignore_ascii_case("permanent")) =>
        {
            HARD_BOUNCE_NOTE
        }
        "email.complained" => COMPLAINT_NOTE,
        _ => return StatusCode::OK,
    };
    for addr in &event.data.to {
        // Channel addresses are stored lowercase; provider echoes may not be.
        let addr = addr.to_ascii_lowercase();
        match state
            .notification_channel_store
            .disable_by_external_ref(ChannelKind::Email, &addr, note)
            .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                disabled = n,
                to = %mask_email(&addr),
                event = %event.kind,
                "email channels disabled by provider event"
            ),
            Err(err) => {
                tracing::warn!(error = %err, "resend webhook: disable failed");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
    }
    StatusCode::OK
}
