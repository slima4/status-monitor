use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ChannelKind;
use super::transport::{MASK, TransportConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
}

impl TransportConfig for TelegramConfig {
    const KIND: ChannelKind = ChannelKind::Telegram;

    fn redact_in_place(&mut self) {
        self.bot_token = MASK.to_string();
    }

    fn has_redaction_sentinel(&self) -> bool {
        self.bot_token == MASK
    }

    fn validate(&self) -> Result<(), String> {
        if self.bot_token.trim().is_empty() {
            return Err("bot_token is required".into());
        }
        if self.chat_id.trim().is_empty() {
            return Err("chat_id is required".into());
        }
        Ok(())
    }

    /// Deliveries go to the fixed api.telegram.org endpoint — no
    /// customer-controlled URL to inspect.
    fn abuse_url(&self) -> Option<&str> {
        None
    }

    fn operator_managed(&self) -> bool {
        false
    }
}
