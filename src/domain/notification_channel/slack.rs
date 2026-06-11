use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ChannelKind;
use super::transport::{MASK, TransportConfig, require_https};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SlackConfig {
    /// Incoming-webhook URL. The path carries the workspace token, so the
    /// whole value is treated as a secret.
    pub webhook_url: String,
}

impl TransportConfig for SlackConfig {
    const KIND: ChannelKind = ChannelKind::Slack;

    fn redact_in_place(&mut self) {
        self.webhook_url = MASK.to_string();
    }

    fn has_redaction_sentinel(&self) -> bool {
        self.webhook_url == MASK
    }

    fn validate(&self) -> Result<(), String> {
        require_https(&self.webhook_url, "webhook_url")
    }

    fn abuse_url(&self) -> Option<&str> {
        Some(&self.webhook_url)
    }

    fn operator_managed(&self) -> bool {
        false
    }
}
