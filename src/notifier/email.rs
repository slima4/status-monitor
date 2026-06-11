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

pub struct EmailNotifier {
    delivery: EmailDelivery,
    to: String,
}

impl EmailNotifier {
    pub fn new(delivery: &EmailDelivery, to: &str) -> Self {
        Self {
            delivery: delivery.clone(),
            to: to.to_string(),
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
