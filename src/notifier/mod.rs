pub mod engine;
pub mod event;
pub mod slack;
pub mod telegram;
pub mod webhook;

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::ChannelConfig;
use crate::error::Result;
use crate::http_outbound::OutboundHttpClient;
use crate::notifier::event::AlertEvent;
use crate::notifier::slack::SlackNotifier;
use crate::notifier::telegram::TelegramNotifier;
use crate::notifier::webhook::WebhookNotifier;

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, event: &AlertEvent) -> Result<()>;
}

/// The single extensibility seam: map a stored [`ChannelConfig`] to its
/// transport. Adding a channel type is one new arm here plus one `Notifier`
/// impl. URLs were validated `https` on channel create; re-parsing here is a
/// defence-in-depth guard, not the primary check.
pub fn build_notifier(cfg: &ChannelConfig, http: &OutboundHttpClient) -> Result<Arc<dyn Notifier>> {
    let parse = |s: &str| -> Result<url::Url> {
        s.parse::<url::Url>().map_err(|e| {
            crate::error::AppError::bad_request(
                crate::api::codes::INVALID_CONFIG,
                format!("notification channel URL is invalid: {e}"),
            )
        })
    };
    Ok(match cfg {
        ChannelConfig::Webhook { url, headers } => Arc::new(WebhookNotifier::new(
            http.clone(),
            parse(url)?,
            headers.clone(),
        )) as Arc<dyn Notifier>,
        ChannelConfig::Slack { webhook_url } => {
            Arc::new(SlackNotifier::new(http.clone(), parse(webhook_url)?)) as Arc<dyn Notifier>
        }
        ChannelConfig::Telegram { bot_token, chat_id } => Arc::new(TelegramNotifier::new(
            http.clone(),
            bot_token,
            chat_id.clone(),
        )?) as Arc<dyn Notifier>,
    })
}
