//! Public status-page subscribers: end users who opt in to incident and
//! maintenance notifications. Distinct from `notification_channel` (operator
//! alerting) — every subscription here is public and double opt-in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscriberChannel {
    Email,
    Webhook,
    Slack,
    Sms,
}

impl SubscriberChannel {
    /// Declaration order; the enum-drift test pins this against the live
    /// `status_page_subscribers.channel` CHECK constraint.
    pub const ALL: &'static [Self] = &[Self::Email, Self::Webhook, Self::Slack, Self::Sms];

    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Webhook => "webhook",
            Self::Slack => "slack",
            Self::Sms => "sms",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_db_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscriber {
    pub id: Uuid,
    pub status_page_id: Uuid,
    pub org_id: Uuid,
    pub channel: SubscriberChannel,
    pub target: String,
    pub config: Value,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Subscriber {
    pub fn is_verified(&self) -> bool {
        self.verified_at.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSubscriber {
    pub status_page_id: Uuid,
    pub org_id: Uuid,
    pub channel: SubscriberChannel,
    pub target: String,
    pub config: Value,
}
