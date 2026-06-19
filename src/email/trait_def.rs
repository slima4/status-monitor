use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

use super::templates;

pub type EmailResult<T> = Result<T, EmailError>;

#[derive(Debug, Error)]
pub enum EmailError {
    #[error("email provider rejected: {0}")]
    ProviderRejected(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid configuration: {0}")]
    Config(String),
}

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, email: TransactionalEmail) -> EmailResult<MessageId>;
}

#[derive(Debug, Clone)]
pub struct EmailAddress {
    pub address: String,
    pub name: String,
}

impl EmailAddress {
    pub fn new(address: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            name: name.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransactionalEmail {
    pub to: EmailAddress,
    pub from: EmailAddress,
    pub template: EmailTemplate,
}

#[derive(Debug, Clone)]
pub enum EmailTemplate {
    Invitation {
        org_name: String,
        inviter_display: String,
        accept_url: String,
        decline_url: String,
        expires_at: DateTime<Utc>,
    },
    /// Schema lands now so the enum is stable; flow is gated by
    /// `auth.enabled_methods` config until the verify endpoint is wired.
    MagicLink {
        url: String,
        expires_in_minutes: u32,
        ip_hint: Option<String>,
    },
    /// Account-deletion notification. Restoring is done by signing in again
    /// before the data is permanently purged on `scheduled_purge_at`.
    AccountDeletion { scheduled_purge_at: DateTime<Utc> },
    /// Confirms an `email` notification channel's address before any alert
    /// is delivered to it.
    ChannelVerification {
        channel_name: String,
        verify_url: String,
        expires_hours: u32,
    },
    /// An incident page delivered over email; `body` is the same plain text
    /// the chat transports send.
    IncidentAlert { body: String },
    /// Confirms a public status-page subscription (double opt-in) before any
    /// update is delivered to the address.
    SubscriberConfirm {
        page_name: String,
        confirm_url: String,
        expires_hours: u32,
    },
    /// A public incident update delivered to a confirmed subscriber.
    SubscriberIncident {
        page_name: String,
        incident_title: String,
        phase: String,
        message: String,
        incident_url: String,
        unsubscribe_url: String,
    },
}

impl EmailTemplate {
    pub fn render(&self, site_name: &str) -> RenderedEmail {
        match self {
            EmailTemplate::Invitation {
                org_name,
                inviter_display,
                accept_url,
                decline_url,
                expires_at,
            } => templates::invitation::render(
                site_name,
                org_name,
                inviter_display,
                accept_url,
                decline_url,
                *expires_at,
            ),
            EmailTemplate::MagicLink {
                url,
                expires_in_minutes,
                ip_hint,
            } => templates::magic_link::render(
                site_name,
                url,
                *expires_in_minutes,
                ip_hint.as_deref(),
            ),
            EmailTemplate::AccountDeletion { scheduled_purge_at } => {
                templates::account_deletion::render(site_name, *scheduled_purge_at)
            }
            EmailTemplate::ChannelVerification {
                channel_name,
                verify_url,
                expires_hours,
            } => templates::channel_verification::render(
                site_name,
                channel_name,
                verify_url,
                *expires_hours,
            ),
            EmailTemplate::IncidentAlert { body } => {
                templates::incident_alert::render(site_name, body)
            }
            EmailTemplate::SubscriberConfirm {
                page_name,
                confirm_url,
                expires_hours,
            } => templates::subscriber_confirm::render(
                site_name,
                page_name,
                confirm_url,
                *expires_hours,
            ),
            EmailTemplate::SubscriberIncident {
                page_name,
                incident_title,
                phase,
                message,
                incident_url,
                unsubscribe_url,
            } => templates::subscriber_incident::render(
                page_name,
                incident_title,
                phase,
                message,
                incident_url,
                unsubscribe_url,
            ),
        }
    }

    /// Action URL surfaced by log-only sender for copy-paste-driven dev flows.
    pub fn primary_url(&self) -> Option<&str> {
        match self {
            EmailTemplate::Invitation { accept_url, .. } => Some(accept_url),
            EmailTemplate::MagicLink { url, .. } => Some(url),
            EmailTemplate::AccountDeletion { .. } => None,
            EmailTemplate::ChannelVerification { verify_url, .. } => Some(verify_url),
            EmailTemplate::IncidentAlert { .. } => None,
            EmailTemplate::SubscriberConfirm { confirm_url, .. } => Some(confirm_url),
            EmailTemplate::SubscriberIncident { incident_url, .. } => Some(incident_url),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedEmail {
    pub subject: String,
    pub text_body: String,
    pub html_body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageId(pub String);

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
