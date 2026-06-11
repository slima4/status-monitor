use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::{AppError, Result};
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;
use crate::telegram::TelegramSendBudget;

/// Telegram Bot API sender. The bot token is embedded in the fixed
/// `api.telegram.org` endpoint path; `chat_id` is sent in the body.
/// `budget` is set only for the central bot (shared across orgs); BYO bots
/// have their own per-customer budget and go unmetered.
pub struct TelegramNotifier {
    client: OutboundHttpClient,
    send_url: Url,
    chat_id: String,
    budget: Option<Arc<TelegramSendBudget>>,
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
            budget: None,
        })
    }

    pub fn with_budget(mut self, budget: Arc<TelegramSendBudget>) -> Self {
        self.budget = Some(budget);
        self
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        if let Some(budget) = &self.budget {
            let chat = self.chat_id.parse::<i64>().unwrap_or_default();
            // The `"retry_after":N` fragment rides the same engine path as a
            // vendor 429 hint, scheduling the retry instead of burning the
            // ceiling — the send never reached Telegram.
            budget.acquire(chat).await.map_err(|d| {
                AppError::Other(anyhow::anyhow!(
                    "telegram send deferred by the local bot budget: {{\"retry_after\":{}}}",
                    d.retry_after_secs
                ))
            })?;
        }
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
