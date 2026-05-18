//! Per-org notification channels. A channel is a named, typed delivery
//! destination (Slack hook, generic webhook, Telegram bot, …) that targets
//! bind to for Down/Recovered alerts.
//!
//! Extensibility seam: adding a transport is one [`ChannelConfig`] variant
//! here, one `Notifier` impl, and one registry arm (Phase 2). The whole
//! config blob is sealed at rest by the credentials KEK at the storage edge,
//! and secrets are never echoed back by the API — see [`ChannelConfig::redacted`].

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Webhook,
    Slack,
    Telegram,
}

impl ChannelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Slack => "slack",
            Self::Telegram => "telegram",
        }
    }
}

/// Transport config, `type`-tagged. Stored sealed at rest; the in-memory
/// domain value is always plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelConfig {
    Webhook {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    Slack {
        webhook_url: String,
    },
    Telegram {
        bot_token: String,
        chat_id: String,
    },
}

const MASK: &str = "***";

impl ChannelConfig {
    pub fn kind(&self) -> ChannelKind {
        match self {
            Self::Webhook { .. } => ChannelKind::Webhook,
            Self::Slack { .. } => ChannelKind::Slack,
            Self::Telegram { .. } => ChannelKind::Telegram,
        }
    }

    /// Overwrite every secret-bearing field with [`MASK`] in place. Non-secret
    /// routing shape (kind, header *names*, chat id) is kept so the UI can
    /// still show which channel this is. The webhook URL itself can carry a
    /// token (…/hooks/T/B/secret), so the whole value is masked.
    ///
    /// Single source of the masking policy: [`Self::redacted`] and the API
    /// `RedactInPlace` impl both route through here, so a new secret field on
    /// a variant is masked everywhere by editing one match arm.
    pub fn redact_in_place(&mut self) {
        match self {
            Self::Webhook { url, headers } => {
                *url = MASK.to_string();
                for v in headers.values_mut() {
                    *v = MASK.to_string();
                }
            }
            Self::Slack { webhook_url } => *webhook_url = MASK.to_string(),
            Self::Telegram { bot_token, .. } => *bot_token = MASK.to_string(),
        }
    }

    /// JSON copy with every secret-bearing field masked, for API responses
    /// and the edit form.
    pub fn redacted(&self) -> serde_json::Value {
        let mut c = self.clone();
        c.redact_in_place();
        serde_json::to_value(&c).unwrap_or(serde_json::Value::Null)
    }

    /// True if any secret-bearing field still carries the redaction sentinel.
    /// A `GET → PATCH` round-trip that re-submits a masked config must be
    /// rejected, never written back as the literal `***`.
    pub fn has_redaction_sentinel(&self) -> bool {
        match self {
            Self::Webhook { url, headers } => url == MASK || headers.values().any(|v| v == MASK),
            Self::Slack { webhook_url } => webhook_url == MASK,
            Self::Telegram { bot_token, .. } => bot_token == MASK,
        }
    }

    /// Cheap structural validation (no network). Returns a human message on
    /// the first problem. Reachability / SSRF checks belong to the notifier
    /// transport (Phase 2), not here.
    pub fn validate(&self) -> Result<(), String> {
        fn https(u: &str, field: &str) -> Result<(), String> {
            let parsed = url::Url::parse(u).map_err(|_| format!("{field} is not a valid URL"))?;
            if parsed.scheme() != "https" {
                return Err(format!("{field} must be an https:// URL"));
            }
            Ok(())
        }
        match self {
            Self::Webhook { url, .. } => https(url, "url"),
            Self::Slack { webhook_url } => https(webhook_url, "webhook_url"),
            Self::Telegram { bot_token, chat_id } => {
                if bot_token.trim().is_empty() {
                    return Err("bot_token is required".into());
                }
                if chat_id.trim().is_empty() {
                    return Err("chat_id is required".into());
                }
                Ok(())
            }
        }
    }
}

pub const MAX_CHANNEL_NAME_LEN: usize = 100;

/// Trimmed-name rule shared by create and update so both reject the same set.
pub fn validate_channel_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("name is required".into());
    }
    if n.chars().count() > MAX_CHANNEL_NAME_LEN {
        return Err(format!(
            "name must be at most {MAX_CHANNEL_NAME_LEN} characters"
        ));
    }
    Ok(())
}

/// Org scoping is implicit via the store (`default_org_id`), exactly like
/// [`crate::domain::MaintenanceWindow`] — no `org_id` field on the wire type.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NotificationChannel {
    pub id: Uuid,
    pub name: String,
    pub kind: ChannelKind,
    /// Always plaintext in memory; the store seals/opens it at the DB edge.
    pub config: ChannelConfig,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewNotificationChannel {
    #[schema(example = "Ops Slack", max_length = 100)]
    pub name: String,
    pub config: ChannelConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NotificationChannelUpdate {
    pub name: Option<String>,
    pub config: Option<ChannelConfig>,
    pub enabled: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_per_variant() {
        for json in [
            r#"{"type":"webhook","url":"https://x.test/h","headers":{"X-Tok":"s"}}"#,
            r#"{"type":"slack","webhook_url":"https://hooks.slack.com/x"}"#,
            r#"{"type":"telegram","bot_token":"123:abc","chat_id":"-100"}"#,
        ] {
            let c: ChannelConfig = serde_json::from_str(json).unwrap();
            let back = serde_json::to_string(&c).unwrap();
            let c2: ChannelConfig = serde_json::from_str(&back).unwrap();
            assert_eq!(c, c2);
        }
    }

    #[test]
    fn kind_matches_variant() {
        let c: ChannelConfig =
            serde_json::from_str(r#"{"type":"telegram","bot_token":"t","chat_id":"1"}"#).unwrap();
        assert_eq!(c.kind(), ChannelKind::Telegram);
        assert_eq!(c.kind().as_str(), "telegram");
    }

    #[test]
    fn redacted_masks_every_secret() {
        let c = ChannelConfig::Telegram {
            bot_token: "123:supersecret".into(),
            chat_id: "-100".into(),
        };
        let r = c.redacted();
        assert_eq!(r["bot_token"], "***");
        // Non-secret routing info is preserved so the UI stays useful.
        assert_eq!(r["chat_id"], "-100");
        assert!(!serde_json::to_string(&r).unwrap().contains("supersecret"));

        let w = ChannelConfig::Webhook {
            url: "https://x.test/hooks/abc/secret".into(),
            headers: BTreeMap::from([("Authorization".into(), "Bearer tkn".into())]),
        };
        let rw = w.redacted();
        assert_eq!(rw["url"], "***");
        assert_eq!(rw["headers"]["Authorization"], "***");
        assert!(!serde_json::to_string(&rw).unwrap().contains("tkn"));
    }

    #[test]
    fn validate_rejects_non_https_and_empty() {
        assert!(
            ChannelConfig::Slack {
                webhook_url: "http://insecure".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            ChannelConfig::Telegram {
                bot_token: "  ".into(),
                chat_id: "1".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            ChannelConfig::Webhook {
                url: "https://ok.test".into(),
                headers: BTreeMap::new()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn mask_matches_canonical_redaction_sentinel() {
        // A redacted config round-tripped through the API must be detectable
        // as the sentinel (Phase 3 rejects re-submitted "***"). That only
        // holds if this mask stays byte-equal to the canonical one.
        assert_eq!(MASK, crate::api::redaction::REDACTED);
    }

    #[test]
    fn name_validation_bounds() {
        assert!(validate_channel_name("  ").is_err());
        assert!(validate_channel_name(&"x".repeat(MAX_CHANNEL_NAME_LEN + 1)).is_err());
        assert!(validate_channel_name("Ops Slack").is_ok());
    }
}
