use std::sync::Arc;

use async_trait::async_trait;

use crate::email::{EmailAddress, EmailSender, EmailTemplate, TransactionalEmail};
use crate::error::Result;
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;

/// Transactional-mail context for alert delivery: the process-wide sender
/// plus the product's From identity. Owned by long-lived senders (engine,
/// app state); `None` at a build site fails the email transport loudly.
#[derive(Clone)]
pub struct EmailDelivery {
    pub sender: Arc<dyn EmailSender>,
    pub from_address: String,
    pub from_name: String,
}

/// Per-send attribution for alert mail: the sending org's name and the
/// recipient's one-click stop link. Absent for test sends.
#[derive(Default)]
pub struct EmailAlert {
    pub org_name: Option<String>,
    pub stop_url: Option<String>,
}

/// Attribution + stop link for an email channel; `None` for every other
/// transport, so the org lookup is skipped unless the recipient is an inbox.
/// Shared by the escalation engine and the silence sweep so both alert streams
/// carry the same footer.
pub async fn email_alert_for(
    orgs: &dyn crate::storage::orgs::OrgDirectory,
    base_url: &str,
    stop_secret: &str,
    org: crate::domain::OrgId,
    channel: &crate::domain::NotificationChannel,
) -> Option<EmailAlert> {
    if channel.kind != crate::domain::ChannelKind::Email {
        return None;
    }
    Some(EmailAlert {
        org_name: orgs.display_name(org).await.ok().flatten(),
        stop_url: crate::storage::notification_channels::channel_stop_url(
            base_url,
            stop_secret,
            channel.id,
        ),
    })
}

pub struct EmailNotifier {
    delivery: EmailDelivery,
    to: String,
    alert: EmailAlert,
}

impl EmailNotifier {
    pub fn new(delivery: &EmailDelivery, to: &str, alert: EmailAlert) -> Self {
        Self {
            delivery: delivery.clone(),
            to: to.to_string(),
            alert,
        }
    }
}

#[async_trait]
impl Notifier for EmailNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let outgoing = TransactionalEmail {
            from: EmailAddress::new(
                self.delivery.from_address.clone(),
                self.delivery.from_name.clone(),
            ),
            to: EmailAddress::new(self.to.clone(), self.to.clone()),
            template: EmailTemplate::IncidentAlert {
                body: notice.plain_text(),
                org_name: self.alert.org_name.clone(),
                stop_url: self.alert.stop_url.clone(),
            },
        };
        self.delivery
            .sender
            .send(outgoing)
            .await
            .map(|_| ())
            .map_err(|e| {
                crate::error::AppError::Other(anyhow::anyhow!("email delivery failed: {e}"))
            })
    }
}
