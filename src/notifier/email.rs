use anyhow::Context;
use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use secrecy::ExposeSecret;

use crate::config::EmailConfig;
use crate::domain::AlertChannel;
use crate::error::{AppError, Result};
use crate::notifier::Notifier;
use crate::notifier::event::{AlertEvent, AlertKind};

pub struct EmailNotifier {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl EmailNotifier {
    pub fn new(cfg: &EmailConfig) -> Result<Self> {
        let from: Mailbox = cfg.from.parse().map_err(|e| {
            AppError::bad_request(
                crate::api::codes::INVALID_CONFIG,
                format!("notifications.email.from: {e}"),
            )
        })?;

        let mut builder = if cfg.starttls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)
                .context("configuring SMTP STARTTLS relay")?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host)
                .context("configuring SMTP TLS relay")?
        }
        .port(cfg.smtp_port);

        if !cfg.smtp_user.is_empty() {
            builder = builder.credentials(Credentials::new(
                cfg.smtp_user.clone(),
                cfg.smtp_password.expose_secret().to_owned(),
            ));
        }

        Ok(Self {
            transport: builder.build(),
            from,
        })
    }

    fn render(event: &AlertEvent) -> (String, String) {
        let subject = match event.kind {
            AlertKind::Down => format!("[status-monitor] {} is DOWN", event.target_name),
            AlertKind::Recovered => format!("[status-monitor] {} has recovered", event.target_name),
        };
        let body = match event.kind {
            AlertKind::Down => format!(
                "{name} has failed {failures} consecutive checks.\nStatus: {status}\nError: {err}\nTimestamp: {ts}\n",
                name = event.target_name,
                failures = event.consecutive_failures,
                status = event.last_status.as_str(),
                err = event.last_error.as_deref().unwrap_or("-"),
                ts = event.timestamp,
            ),
            AlertKind::Recovered => format!(
                "{name} has returned to UP.\nTimestamp: {ts}\n",
                name = event.target_name,
                ts = event.timestamp,
            ),
        };
        (subject, body)
    }
}

#[async_trait]
impl Notifier for EmailNotifier {
    fn channel(&self) -> AlertChannel {
        AlertChannel::Email
    }

    async fn notify(&self, event: &AlertEvent) -> Result<()> {
        if event.recipients.is_empty() {
            return Err(AppError::bad_request(
                crate::api::codes::INVALID_CONFIG,
                "email notifier invoked with empty recipients",
            ));
        }
        let (subject, body) = Self::render(event);
        for to in &event.recipients {
            let to_mbox: Mailbox = to.parse().map_err(|e| {
                AppError::bad_request(
                    crate::api::codes::INVALID_CONFIG,
                    format!("email recipient '{to}': {e}"),
                )
            })?;
            let msg = Message::builder()
                .from(self.from.clone())
                .to(to_mbox)
                .subject(subject.clone())
                .body(body.clone())
                .context("building email message")?;
            self.transport.send(msg).await.context("sending email")?;
        }
        Ok(())
    }
}
