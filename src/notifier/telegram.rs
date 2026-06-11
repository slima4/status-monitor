use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::{AppError, Result};
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;

/// Telegram Bot API sender. The bot token is embedded in the fixed
/// `api.telegram.org` endpoint path; `chat_id` is sent in the body.
pub struct TelegramNotifier {
    client: OutboundHttpClient,
    send_url: Url,
    chat_id: String,
}

#[derive(Serialize)]
struct SendMessage<'a> {
    chat_id: &'a str,
    text: &'a str,
}

impl TelegramNotifier {
    pub fn new(client: OutboundHttpClient, bot_token: &str, chat_id: String) -> Result<Self> {
        // Host is fixed; only the token (already validated non-empty on
        // channel create) varies. Parsing guards against a token with URL
        // metacharacters reaching the path.
        let send_url = format!("https://api.telegram.org/bot{bot_token}/sendMessage")
            .parse::<Url>()
            .map_err(|e| {
                AppError::bad_request(
                    crate::api::codes::INVALID_CONFIG,
                    format!("telegram bot_token is not URL-safe: {e}"),
                )
            })?;
        Ok(Self {
            client,
            send_url,
            chat_id,
        })
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let text = notice.plain_text();
        post_json(
            &self.client,
            &self.send_url,
            &SendMessage {
                chat_id: &self.chat_id,
                text: &text,
            },
        )
        .await
    }
}
