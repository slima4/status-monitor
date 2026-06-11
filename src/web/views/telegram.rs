//! Inbound webhook receiver for the central Telegram bot (`/hooks/telegram`).
//!
//! Telegram has no signature scheme, so the only auth is the
//! `X-Telegram-Bot-Api-Secret-Token` header echoing the configured secret,
//! compared in constant time. Every accepted update is answered 200 fast:
//! Telegram retries non-2xx aggressively, so a malformed or unhandled update
//! is still acknowledged.
//!
//! A `/start <code>` (or `/link <code>`) message claims the link code and
//! creates the org's `telegram_app` channel. The org is taken exclusively
//! from the consumed code row — never from the update body. The in-chat
//! reply goes out on a background task so a slow Telegram API can't delay
//! the acknowledgement.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use secrecy::ExposeSecret;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::sha256_hex;
use crate::domain::{
    ChannelConfig, MAX_CHANNEL_NAME_LEN, NewNotificationChannel, NotificationChannel, OrgId,
    TelegramAppConfig, WriteSource,
};
use crate::error::{AppError, Result};
use crate::storage::NotificationChannelStore;
use crate::telegram::{
    ChatRef, TelegramClient, Update, WebhookAction, classify_update, webhook_secret_matches,
};

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

    let update = match serde_json::from_slice::<Update>(&body) {
        Ok(update) => update,
        Err(err) => {
            tracing::warn!(?err, "telegram webhook: unparseable update body");
            return StatusCode::OK;
        }
    };
    match classify_update(&update) {
        WebhookAction::LinkPrivate { code, chat } | WebhookAction::LinkGroup { code, chat } => {
            // All the work happens off the request: the ack must not wait on
            // the store or the advisory lock, or Telegram re-delivers and the
            // retry races the first attempt's consume.
            tokio::spawn(async move { handle_link(&state, &code, chat).await });
        }
        WebhookAction::Removed { chat_id } => {
            tracing::debug!(chat_id, "telegram bot removed from chat");
        }
        WebhookAction::Ignore => {}
    }
    StatusCode::OK
}

/// Claim the code and create the channel, then answer in-chat. Failures are
/// logged and answered politely; the webhook still acknowledges 200 either
/// way, so Telegram never retries a consumed code at us.
async fn handle_link(state: &AppState, code: &str, chat: ChatRef) {
    let chat_id = chat.id;
    let text = match link_chat(state, code, chat).await {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(?err, chat_id, "telegram link consume failed");
            "Something went wrong while linking. Please mint a fresh link and try again."
                .to_string()
        }
    };
    spawn_reply(state, chat_id, text);
}

async fn link_chat(state: &AppState, code: &str, chat: ChatRef) -> Result<String> {
    let Some(link) = state
        .telegram_link_code_store
        .consume(&sha256_hex(code))
        .await?
    else {
        return Ok(
            "This link is invalid, expired, or already used. Mint a fresh link from the \
             notification settings and try again."
                .to_string(),
        );
    };
    let org = link.org_id;

    let base_name = link
        .channel_name
        .as_deref()
        .or(chat.title.as_deref())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Telegram");
    let limit = i64::from(
        state
            .quotas
            .limit_for_org(org)
            .await?
            .max_notification_channels,
    );
    let config = ChannelConfig::TelegramApp(TelegramAppConfig {
        chat_id: chat.id.to_string(),
        chat_title: chat.title.clone(),
    });
    let channel = match create_linked_channel(
        state.notification_channel_store.as_ref(),
        org,
        base_name,
        config,
        limit,
    )
    .await
    {
        Ok(ch) => ch,
        Err(AppError::Unprocessable { code, .. }) if code == codes::CHANNEL_QUOTA_EXCEEDED => {
            return Ok(
                "Couldn't link: this workspace is at its notification-channel limit. \
                 Remove an unused channel and mint a fresh link."
                    .to_string(),
            );
        }
        Err(err) => return Err(err),
    };
    if let Err(err) = state
        .telegram_link_code_store
        .attach_channel(link.id, channel.id)
        .await
    {
        // The channel exists and works; only the form's poll misses out.
        tracing::warn!(?err, channel_id = %channel.id, "telegram link attach failed");
    }

    let org_name = match &state.db {
        Some(pool) => crate::storage::get_org(pool, org)
            .await
            .ok()
            .flatten()
            .map(|o| o.name),
        None => None,
    };
    Ok(match org_name {
        Some(name) => format!("Linked to {name} — alerts will arrive in this chat."),
        None => "Linked — alerts will arrive in this chat.".to_string(),
    })
}

/// Create the consume-path channel, deduping the name against
/// `CHANNEL_NAME_TAKEN` by suffixing (`Ops`, `Ops 2`, `Ops 3`, …). Any other
/// error propagates unchanged.
pub async fn create_linked_channel(
    store: &dyn NotificationChannelStore,
    org: OrgId,
    base_name: &str,
    config: ChannelConfig,
    max_channels: i64,
) -> Result<NotificationChannel> {
    const MAX_SUFFIX: u32 = 50;
    let mut attempt = 1;
    loop {
        let suffix = (attempt > 1).then_some(attempt);
        let new = NewNotificationChannel {
            name: linked_channel_name(base_name, suffix),
            config: config.clone(),
            enabled: true,
        };
        match store.create(org, new, WriteSource::Ui, max_channels).await {
            Err(AppError::Unprocessable { code, .. })
                if code == codes::CHANNEL_NAME_TAKEN && attempt < MAX_SUFFIX =>
            {
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// `base` (optionally `base N`), trimmed to the channel-name length budget so
/// a long group title still leaves room for the dedupe suffix.
pub fn linked_channel_name(base: &str, suffix: Option<u32>) -> String {
    let suffix = suffix.map(|n| format!(" {n}")).unwrap_or_default();
    let budget = MAX_CHANNEL_NAME_LEN - suffix.chars().count();
    let base: String = base.trim().chars().take(budget).collect();
    format!("{}{suffix}", base.trim_end())
}

fn spawn_reply(state: &AppState, chat_id: i64, text: String) {
    let client = TelegramClient::new(
        state.outbound_http.clone(),
        state.cfg.telegram.bot_token.expose_secret(),
    );
    tokio::spawn(async move {
        if let Err(err) = client.send_message(chat_id, &text).await {
            tracing::warn!(?err, chat_id, "telegram link reply failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ChannelKind;
    use crate::storage::InMemoryNotificationChannelStore;
    use uuid::Uuid;

    fn org() -> OrgId {
        OrgId(Uuid::from_u128(0xC3))
    }

    fn app_config(chat_id: &str) -> ChannelConfig {
        ChannelConfig::TelegramApp(TelegramAppConfig {
            chat_id: chat_id.into(),
            chat_title: Some("Ops".into()),
        })
    }

    #[tokio::test]
    async fn linked_channel_dedupes_name_with_suffix() {
        let store = InMemoryNotificationChannelStore::new();
        for expected in ["Ops", "Ops 2", "Ops 3"] {
            let ch = create_linked_channel(&store, org(), "Ops", app_config("-1"), 10)
                .await
                .unwrap();
            assert_eq!(ch.name, expected);
            assert_eq!(ch.kind, ChannelKind::TelegramApp);
            assert!(ch.enabled);
        }
    }

    #[tokio::test]
    async fn linked_channel_quota_error_passes_through() {
        let store = InMemoryNotificationChannelStore::new();
        create_linked_channel(&store, org(), "Ops", app_config("-1"), 1)
            .await
            .unwrap();
        let err = create_linked_channel(&store, org(), "Other", app_config("-2"), 1)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AppError::Unprocessable { code, .. } if code == codes::CHANNEL_QUOTA_EXCEEDED)
        );
    }

    #[test]
    fn name_budget_keeps_room_for_suffix() {
        let long = "x".repeat(MAX_CHANNEL_NAME_LEN + 20);
        let plain = linked_channel_name(&long, None);
        assert_eq!(plain.chars().count(), MAX_CHANNEL_NAME_LEN);
        let suffixed = linked_channel_name(&long, Some(12));
        assert_eq!(suffixed.chars().count(), MAX_CHANNEL_NAME_LEN);
        assert!(suffixed.ends_with(" 12"));
        assert_eq!(linked_channel_name("  Ops  ", Some(2)), "Ops 2");
    }
}
