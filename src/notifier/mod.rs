pub mod event;
pub mod slack;
pub mod telegram;
pub mod webhook;

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::ChannelConfig;
use crate::error::Result;
use crate::http_outbound::OutboundHttpClient;
use crate::notifier::event::IncidentNotice;
use crate::notifier::slack::SlackNotifier;
use crate::notifier::telegram::TelegramNotifier;
use crate::notifier::webhook::WebhookNotifier;

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Page an incident lifecycle event (opened/resolved/reopened/escalated).
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()>;
}

/// Delivery-side factory: map a stored [`ChannelConfig`] to its transport.
/// The full add-a-transport checklist lives on
/// `crate::domain::notification_channel`. URLs were validated `https` on
/// channel create; re-parsing here is a defence-in-depth guard, not the
/// primary check.
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
        ChannelConfig::Webhook(c) => Arc::new(WebhookNotifier::new(
            http.clone(),
            parse(&c.url)?,
            c.headers.clone(),
            c.secret.clone(),
        )) as Arc<dyn Notifier>,
        ChannelConfig::Slack(c) => {
            Arc::new(SlackNotifier::new(http.clone(), parse(&c.webhook_url)?)) as Arc<dyn Notifier>
        }
        ChannelConfig::Telegram(c) => Arc::new(TelegramNotifier::new(
            http.clone(),
            &c.bot_token,
            c.chat_id.clone(),
        )?) as Arc<dyn Notifier>,
    })
}
