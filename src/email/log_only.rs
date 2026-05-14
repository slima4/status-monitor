use async_trait::async_trait;
use uuid::Uuid;

use super::trait_def::{EmailResult, EmailSender, MessageId, TransactionalEmail};

pub struct LogOnlyEmailSender {
    site_name: String,
}

impl LogOnlyEmailSender {
    pub fn new(site_name: impl Into<String>) -> Self {
        Self {
            site_name: site_name.into(),
        }
    }
}

impl Default for LogOnlyEmailSender {
    fn default() -> Self {
        Self::new("Status Monitor [DEV]")
    }
}

#[async_trait]
impl EmailSender for LogOnlyEmailSender {
    async fn send(&self, email: TransactionalEmail) -> EmailResult<MessageId> {
        let rendered = email.template.render(&self.site_name);
        tracing::info!(
            to = %email.to.address,
            subject = %rendered.subject,
            "📧 EMAIL (not actually sent — log-only mode)"
        );
        tracing::debug!(text_body = %rendered.text_body);

        if let Some(url) = email.template.primary_url() {
            tracing::info!("📧 Action URL: {url}");
        }

        Ok(MessageId(format!("log-only-{}", Uuid::now_v7())))
    }
}
