pub mod email;
pub mod engine;
pub mod event;
pub mod slack;
pub mod transport;
pub mod webhook;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::NotificationsConfig;
use crate::domain::AlertChannel;
use crate::error::{AppError, Result};
use crate::notifier::email::EmailNotifier;
use crate::notifier::event::AlertEvent;
use crate::notifier::slack::SlackNotifier;
use crate::notifier::transport::{NotifyHttpClient, build_notify_client};
use crate::notifier::webhook::WebhookNotifier;

#[async_trait]
pub trait Notifier: Send + Sync {
    fn channel(&self) -> AlertChannel;
    async fn notify(&self, event: &AlertEvent) -> Result<()>;
}

/// Builds the set of globally-enabled notifiers from config. Per-target opt-ins
/// that reference a channel missing from this map are logged and dropped by the
/// engine.
pub fn build_notifiers(cfg: &NotificationsConfig) -> Result<Vec<Arc<dyn Notifier>>> {
    let mut out: Vec<Arc<dyn Notifier>> = Vec::new();
    let mut http_client: Option<NotifyHttpClient> = None;
    if cfg.slack.enabled {
        if cfg.slack.webhook_url.is_empty() {
            return Err(AppError::BadRequest(
                "notifications.slack.enabled = true requires webhook_url".into(),
            ));
        }
        let url =
            cfg.slack.webhook_url.parse::<url::Url>().map_err(|e| {
                AppError::BadRequest(format!("notifications.slack.webhook_url: {e}"))
            })?;
        let client = http_client.get_or_insert_with(build_notify_client).clone();
        out.push(Arc::new(SlackNotifier::new(client, url)) as Arc<dyn Notifier>);
    }
    if cfg.webhook.enabled {
        if cfg.webhook.url.is_empty() {
            return Err(AppError::BadRequest(
                "notifications.webhook.enabled = true requires url".into(),
            ));
        }
        let url = cfg
            .webhook
            .url
            .parse::<url::Url>()
            .map_err(|e| AppError::BadRequest(format!("notifications.webhook.url: {e}")))?;
        let client = http_client.get_or_insert_with(build_notify_client).clone();
        out.push(Arc::new(WebhookNotifier::new(client, url)) as Arc<dyn Notifier>);
    }
    if cfg.email.enabled {
        // Plaintext SMTP carries the password in the clear during AUTH. Disallow
        // any auth setup that would leak the password over a non-TLS link.
        if !cfg.email.smtp_password.is_empty() && cfg.email.smtp_port == 25 && !cfg.email.starttls {
            return Err(AppError::BadRequest(
                "notifications.email: smtp_password is set but smtp_port=25 with starttls=false would leak the password in cleartext".into(),
            ));
        }
        if cfg.email.smtp_host.is_empty() || cfg.email.from.is_empty() {
            return Err(AppError::BadRequest(
                "notifications.email.enabled = true requires smtp_host and from".into(),
            ));
        }
        out.push(Arc::new(EmailNotifier::new(&cfg.email)?) as Arc<dyn Notifier>);
    }
    Ok(out)
}
