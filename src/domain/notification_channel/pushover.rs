use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ChannelKind;
use super::transport::{MASK, TransportConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PushoverConfig {
    /// Application API token.
    pub token: String,
    /// User or group key. Also masked: possession plus any app token is
    /// enough to message the person.
    pub user: String,
    /// Optional device name; empty/absent delivers to all devices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

fn is_pushover_key(s: &str) -> bool {
    s.len() == 30 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

impl TransportConfig for PushoverConfig {
    const KIND: ChannelKind = ChannelKind::Pushover;

    fn redact_in_place(&mut self) {
        self.token = MASK.to_string();
        self.user = MASK.to_string();
    }

    fn has_redaction_sentinel(&self) -> bool {
        self.token == MASK || self.user == MASK
    }

    fn validate(&self) -> Result<(), String> {
        if !is_pushover_key(&self.token) {
            return Err("token must be a 30-character Pushover application token".into());
        }
        if !is_pushover_key(&self.user) {
            return Err("user must be a 30-character Pushover user or group key".into());
        }
        if let Some(d) = &self.device
            && (d.is_empty()
                || d.len() > 25
                || !d
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        {
            return Err("device must be 1-25 characters of letters, digits, _ or -".into());
        }
        Ok(())
    }

    fn abuse_url(&self) -> Option<&str> {
        None
    }

    fn operator_managed(&self) -> bool {
        false
    }
}
