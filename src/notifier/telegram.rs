use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::domain::NotificationReason;
use crate::error::{AppError, Result};
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::{AlertEvent, AlertKind, IncidentNotice};

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

    fn render(event: &AlertEvent) -> String {
        match event.kind {
            AlertKind::Down => format!(
                "🚨 {name} is DOWN ({failures} consecutive failures, status={status}{err})",
                name = event.target_name,
                failures = event.consecutive_failures,
                status = event.last_status.as_str(),
                err = event
                    .last_error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            ),
            AlertKind::Recovered => format!("✅ {name} has RECOVERED", name = event.target_name),
        }
    }

    fn render_incident(n: &IncidentNotice) -> String {
        let link = n
            .url
            .as_deref()
            .map(|u| format!("\n{u}"))
            .unwrap_or_default();
        match n.reason {
            NotificationReason::Opened | NotificationReason::Escalated => format!(
                "{label} — {sev} incident OPEN{err}{link}",
                label = n.label(),
                sev = n.severity.as_db_str(),
                err = n
                    .error_sample
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default(),
            ),
            NotificationReason::Reopened => {
                format!("{label} — incident REOPENED{link}", label = n.label())
            }
            NotificationReason::Resolved => {
                let dur = n
                    .duration_minutes()
                    .map(|m| format!(" after {m}m"))
                    .unwrap_or_default();
                format!("{label} — incident RESOLVED{dur}{link}", label = n.label())
            }
        }
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify(&self, event: &AlertEvent) -> Result<()> {
        let text = Self::render(event);
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

    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let text = Self::render_incident(notice);
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
