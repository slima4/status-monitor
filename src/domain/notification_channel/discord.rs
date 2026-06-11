use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ChannelKind;
use super::transport::{MASK, TransportConfig, require_provider_webhook};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscordConfig {
    /// Channel webhook URL. The path carries the webhook token, so the
    /// whole value is treated as a secret.
    pub webhook_url: String,
}

impl TransportConfig for DiscordConfig {
    const KIND: ChannelKind = ChannelKind::Discord;

    fn redact_in_place(&mut self) {
        self.webhook_url = MASK.to_string();
    }

    fn has_redaction_sentinel(&self) -> bool {
        self.webhook_url == MASK
    }

    fn validate(&self) -> Result<(), String> {
        require_provider_webhook(
            &self.webhook_url,
            "Discord",
            &["discord.com", "discordapp.com"],
            Some("/api/webhooks/"),
        )
    }

    fn abuse_url(&self) -> Option<&str> {
        Some(&self.webhook_url)
    }

    fn operator_managed(&self) -> bool {
        false
    }
}
