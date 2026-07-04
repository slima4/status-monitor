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
    /// is delivered to it. `org_name` names the org that added the address and
    /// `decline_url` lets the recipient refuse in one click.
    ChannelVerification {
        channel_name: String,
        verify_url: String,
        expires_hours: u32,
        org_name: Option<String>,
        decline_url: Option<String>,
    },
    /// An incident page delivered over email; `body` is the same plain text
    /// the chat transports send. `org_name` attributes the sending org and
    /// `stop_url` is the recipient's one-click opt-out.
    IncidentAlert {
        body: String,
        org_name: Option<String>,
        stop_url: Option<String>,
    },
    /// Confirms a public status-page subscription (double opt-in) before any
    /// update is delivered to the address.
    SubscriberConfirm {
        page_name: String,
        confirm_url: String,
        expires_hours: u32,
        unsubscribe_url: String,
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
    /// A maintenance-window announcement or completion for a confirmed
    /// subscriber. `phase` is `scheduled` or `completed`.
    SubscriberMaintenance {
        page_name: String,
        title: String,
        description: Option<String>,
        phase: String,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
        page_url: String,
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
                org_name,
                decline_url,
            } => templates::channel_verification::render(
                site_name,
                channel_name,
                verify_url,
                *expires_hours,
                org_name.as_deref(),
                decline_url.as_deref(),
            ),
            EmailTemplate::IncidentAlert {
                body,
                org_name,
                stop_url,
            } => templates::incident_alert::render(
                site_name,
                body,
                org_name.as_deref(),
                stop_url.as_deref(),
            ),
            EmailTemplate::SubscriberConfirm {
                page_name,
                confirm_url,
                expires_hours,
                unsubscribe_url,
            } => templates::subscriber_confirm::render(
                site_name,
                page_name,
                confirm_url,
                *expires_hours,
                unsubscribe_url,
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
            EmailTemplate::SubscriberMaintenance {
                page_name,
                title,
                description,
                phase,
                starts_at,
                ends_at,
                page_url,
                unsubscribe_url,
            } => templates::subscriber_maintenance::render(
                page_name,
                title,
                description.as_deref(),
                phase,
                *starts_at,
                *ends_at,
                page_url,
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
            EmailTemplate::SubscriberMaintenance { page_url, .. } => Some(page_url),
        }
    }

    /// One-click unsubscribe URL surfaced as RFC 8058 List-Unsubscribe headers
    /// by the transport. Subscriber mail carries its unsubscribe link; the
    /// still-unverified verification mail carries a one-click decline so a
    /// non-consenting inbox can shut it down at the mail-client level. Incident
    /// alerts deliberately do NOT: one-click is auto-actuated by mail gateways
    /// and native clients, and silently disabling a live paging channel is a
    /// worse failure than a missed unsubscribe. Their stop link lives in the
    /// body, behind a confirmation, so disabling stays a deliberate act.
    pub fn list_unsubscribe_url(&self) -> Option<&str> {
        match self {
            EmailTemplate::SubscriberIncident {
                unsubscribe_url, ..
            }
            | EmailTemplate::SubscriberMaintenance {
                unsubscribe_url, ..
            }
            | EmailTemplate::SubscriberConfirm {
                unsubscribe_url, ..
            } => Some(unsubscribe_url),
            EmailTemplate::ChannelVerification { decline_url, .. } => decline_url.as_deref(),
            _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incident_alert_withholds_one_click_but_verification_offers_it() {
        let alert = EmailTemplate::IncidentAlert {
            body: "x".into(),
            org_name: Some("Acme".into()),
            stop_url: Some("https://app/alert-channel/stop?c=1&t=2".into()),
        };
        assert_eq!(alert.list_unsubscribe_url(), None);

        let verify = EmailTemplate::ChannelVerification {
            channel_name: "c".into(),
            verify_url: "https://app/verify".into(),
            expires_hours: 24,
            org_name: None,
            decline_url: Some("https://app/alert-channel/stop?c=3&t=4".into()),
        };
        assert_eq!(
            verify.list_unsubscribe_url(),
            Some("https://app/alert-channel/stop?c=3&t=4")
        );
    }

    #[test]
    fn subscriber_confirm_offers_one_click_unsubscribe() {
        let confirm = EmailTemplate::SubscriberConfirm {
            page_name: "Acme".into(),
            confirm_url: "https://acme/subscribe/confirm?token=x".into(),
            expires_hours: 24,
            unsubscribe_url: "https://acme/subscribe/unsubscribe?s=1&t=2".into(),
        };
        assert_eq!(
            confirm.list_unsubscribe_url(),
            Some("https://acme/subscribe/unsubscribe?s=1&t=2")
        );
    }
}
