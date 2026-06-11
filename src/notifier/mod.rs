pub mod event;
pub mod slack;
pub mod telegram;
pub mod webhook;
pub mod whatsapp;

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::ChannelConfig;
use crate::error::Result;
use crate::http_outbound::OutboundHttpClient;
use crate::notifier::event::IncidentNotice;
use crate::notifier::slack::SlackNotifier;
use crate::notifier::telegram::TelegramNotifier;
use crate::notifier::webhook::WebhookNotifier;
use crate::notifier::whatsapp::WhatsAppNotifier;

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Page an incident lifecycle event (opened/resolved/reopened/escalated).
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()>;
}

/// Central-bot delivery context for one factory call: operator token plus
/// the process-wide send budget.
#[derive(Clone, Copy)]
pub struct CentralTelegram<'a> {
    pub bot_token: &'a str,
    pub budget: &'a Arc<crate::telegram::TelegramSendBudget>,
}

/// Owned counterpart held by long-lived senders (the escalation engine).
/// The budget Arc must be the process-wide instance — a second instance
/// would double the bot's rate budget.
pub struct CentralBotDelivery {
    pub token: secrecy::SecretString,
    pub budget: Arc<crate::telegram::TelegramSendBudget>,
}

impl CentralBotDelivery {
    pub fn as_central(&self) -> CentralTelegram<'_> {
        use secrecy::ExposeSecret;
        CentralTelegram {
            bot_token: self.token.expose_secret(),
            budget: &self.budget,
        }
    }
}

/// Delivery-side factory: map a stored [`ChannelConfig`] to its transport.
/// The full add-a-transport checklist lives on
/// `crate::domain::notification_channel`. URLs were validated `https` on
/// channel create; re-parsing here is a defence-in-depth guard, not the
/// primary check.
///
/// Linked (`telegram_app`) channels deliver with the operator token and the
/// shared send budget in `central`; `None` (no bot) fails their build with
/// a clear error instead of a broken send.
pub fn build_notifier(
    cfg: &ChannelConfig,
    http: &OutboundHttpClient,
    central: Option<CentralTelegram<'_>>,
) -> Result<Arc<dyn Notifier>> {
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
        ChannelConfig::TelegramApp(c) => {
            let central = central
                .filter(|c| !c.bot_token.trim().is_empty())
                .ok_or_else(|| {
                    crate::error::AppError::bad_request(
                        crate::api::codes::INVALID_CONFIG,
                        "linked telegram channels need the central bot, which is not configured \
                         on this deployment",
                    )
                })?;
            Arc::new(
                TelegramNotifier::new(http.clone(), central.bot_token.trim(), c.chat_id.clone())?
                    .with_budget(central.budget.clone()),
            ) as Arc<dyn Notifier>
        }
        ChannelConfig::WhatsApp(c) => {
            Arc::new(WhatsAppNotifier::new(http.clone(), c)?) as Arc<dyn Notifier>
        }
    })
}
