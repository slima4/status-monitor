//! Per-org notification channels. A channel is a named, typed delivery
//! destination (Slack hook, generic webhook, Telegram bot, …) that targets
//! bind to for Down/Recovered alerts.
//!
//! Each transport is a self-contained module implementing
//! [`TransportConfig`]; [`ChannelConfig`] only delegates. The full
//! add-a-transport checklist:
//!
//! 1. config module here + [`TransportConfig`] impl;
//! 2. [`ChannelKind`] variant, [`ChannelKind::ALL`], `as_db_str`, and the
//!    Postgres `kind` CHECK constraint (the enum-drift test compares them);
//! 3. [`ChannelConfig`] variant + `with_transport!` arm — the compiler then
//!    points at the remaining match sites (`build_notifier`, the form
//!    prefill);
//! 4. a `Notifier` impl in `crate::notifier` and its factory arm;
//! 5. the form UI: template variant panel + type card + JS config builder.
//!
//! The whole config blob is sealed at rest by the credentials KEK at the
//! storage edge, and secrets are never echoed back by the API — see
//! [`ChannelConfig::redacted`].

mod slack;
mod telegram;
mod transport;
mod webhook;
mod whatsapp;

pub use slack::SlackConfig;
pub use telegram::TelegramConfig;
pub use transport::TransportConfig;
pub use webhook::WebhookConfig;
pub use whatsapp::WhatsAppConfig;

use super::WriteSource;
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
    #[serde(rename = "whatsapp")]
    WhatsApp,
}

impl ChannelKind {
    /// Every variant in declaration order. Used by the enum-drift integration
    /// test to compare against the live Postgres CHECK constraint on
    /// `notification_channels.kind`; keep in lockstep with the enum body.
    pub const ALL: &'static [Self] = &[Self::Webhook, Self::Slack, Self::Telegram, Self::WhatsApp];

    /// Stable string used in the Postgres `kind` CHECK constraint and the
    /// JSON wire form.
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Slack => "slack",
            Self::Telegram => "telegram",
            Self::WhatsApp => "whatsapp",
        }
    }
}

/// Transport config, `type`-tagged on the wire (newtype variants flatten the
/// inner struct's fields into the same JSON object). Stored sealed at rest;
/// the in-memory domain value is always plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelConfig {
    Webhook(WebhookConfig),
    Slack(SlackConfig),
    Telegram(TelegramConfig),
    #[serde(rename = "whatsapp")]
    WhatsApp(WhatsAppConfig),
}

/// Apply `$body` to the inner [`TransportConfig`] of any variant. The one
/// place that has to enumerate the variants for delegation.
macro_rules! with_transport {
    ($self:expr, |$c:ident| $body:expr) => {
        match $self {
            ChannelConfig::Webhook($c) => $body,
            ChannelConfig::Slack($c) => $body,
            ChannelConfig::Telegram($c) => $body,
            ChannelConfig::WhatsApp($c) => $body,
        }
    };
}

impl ChannelConfig {
    pub fn kind(&self) -> ChannelKind {
        with_transport!(self, |c| c.kind())
    }

    /// Overwrite every secret-bearing field with the redaction mask in
    /// place. Single source of the masking policy: [`Self::redacted`] and
    /// the API `RedactInPlace` impl both route through here.
    pub fn redact_in_place(&mut self) {
        with_transport!(self, |c| c.redact_in_place())
    }

    /// JSON copy with every secret-bearing field masked, for API responses
    /// and the edit form.
    pub fn redacted(&self) -> serde_json::Value {
        let mut c = self.clone();
        c.redact_in_place();
        serde_json::to_value(&c).unwrap_or(serde_json::Value::Null)
    }

    pub fn has_redaction_sentinel(&self) -> bool {
        with_transport!(self, |c| c.has_redaction_sentinel())
    }

    pub fn validate(&self) -> Result<(), String> {
        with_transport!(self, |c| c.validate())
    }

    /// The customer-controlled destination URL for the abuse deny-list;
    /// `None` for transports with a fixed vendor endpoint.
    pub fn abuse_url(&self) -> Option<&str> {
        with_transport!(self, |c| c.abuse_url())
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

/// No `org_id` on the wire type: the owning org is the caller's resolved
/// tenant, threaded explicitly into every store call, never trusted from the
/// request body.
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
    /// Where this channel was last changed from (UI, API, or Terraform).
    pub write_source: WriteSource,
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
    use std::collections::BTreeMap;

    use super::transport::MASK;
    use super::*;

    #[test]
    fn config_round_trips_per_variant() {
        for json in [
            r#"{"type":"webhook","url":"https://x.test/h","headers":{"X-Tok":"s"}}"#,
            r#"{"type":"webhook","url":"https://x.test/h","secret":"0123456789abcdef"}"#,
            r#"{"type":"slack","webhook_url":"https://hooks.slack.com/x"}"#,
            r#"{"type":"telegram","bot_token":"123:abc","chat_id":"-100"}"#,
            r#"{"type":"whatsapp","access_token":"tok","phone_number_id":"123","to":"15551234567","template_name":"uptime_alert"}"#,
            r#"{"type":"whatsapp","access_token":"tok","phone_number_id":"123","to":"15551234567","template_name":"uptime_alert","language_code":"en"}"#,
        ] {
            let c: ChannelConfig = serde_json::from_str(json).unwrap();
            let back = serde_json::to_string(&c).unwrap();
            let c2: ChannelConfig = serde_json::from_str(&back).unwrap();
            assert_eq!(c, c2);
        }
    }

    #[test]
    fn kind_matches_wire_tag_for_every_variant() {
        // KIND is a per-struct constant, no longer tied to the enum variant
        // by a match — this pins each one to its serde `type` tag so a
        // copy-pasted transport can't desync the DB `kind` column from the
        // stored config.
        let configs = [
            ChannelConfig::Webhook(WebhookConfig {
                url: "https://x.test".into(),
                headers: BTreeMap::new(),
                secret: None,
            }),
            ChannelConfig::Slack(SlackConfig {
                webhook_url: "https://hooks.slack.com/x".into(),
            }),
            ChannelConfig::Telegram(TelegramConfig {
                bot_token: "t".into(),
                chat_id: "1".into(),
            }),
            ChannelConfig::WhatsApp(WhatsAppConfig {
                access_token: "tok".into(),
                phone_number_id: "123".into(),
                to: "15551234567".into(),
                template_name: "uptime_alert".into(),
                language_code: None,
            }),
        ];
        assert_eq!(configs.len(), ChannelKind::ALL.len());
        for c in configs {
            let tag = serde_json::to_value(&c).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(c.kind().as_db_str(), tag);
        }
    }

    #[test]
    fn redacted_masks_every_secret() {
        let c = ChannelConfig::Telegram(TelegramConfig {
            bot_token: "123:supersecret".into(),
            chat_id: "-100".into(),
        });
        let r = c.redacted();
        assert_eq!(r["bot_token"], "***");
        // Non-secret routing info is preserved so the UI stays useful.
        assert_eq!(r["chat_id"], "-100");
        assert!(!serde_json::to_string(&r).unwrap().contains("supersecret"));

        let w = ChannelConfig::Webhook(WebhookConfig {
            url: "https://x.test/hooks/abc/secret".into(),
            headers: BTreeMap::from([("Authorization".into(), "Bearer tkn".into())]),
            secret: Some("0123456789abcdef".into()),
        });
        let rw = w.redacted();
        assert_eq!(rw["url"], "***");
        assert_eq!(rw["headers"]["Authorization"], "***");
        assert_eq!(rw["secret"], "***");
        assert!(!serde_json::to_string(&rw).unwrap().contains("tkn"));
        // A re-submitted masked signing secret is caught as the sentinel.
        let mut masked = w.clone();
        masked.redact_in_place();
        assert!(masked.has_redaction_sentinel());
    }

    #[test]
    fn validate_rejects_non_https_and_empty() {
        assert!(
            ChannelConfig::Slack(SlackConfig {
                webhook_url: "http://insecure".into()
            })
            .validate()
            .is_err()
        );
        assert!(
            ChannelConfig::Telegram(TelegramConfig {
                bot_token: "  ".into(),
                chat_id: "1".into()
            })
            .validate()
            .is_err()
        );
        assert!(
            ChannelConfig::Webhook(WebhookConfig {
                url: "https://ok.test".into(),
                headers: BTreeMap::new(),
                secret: None,
            })
            .validate()
            .is_ok()
        );
        // A too-short signing secret is rejected.
        assert!(
            ChannelConfig::Webhook(WebhookConfig {
                url: "https://ok.test".into(),
                headers: BTreeMap::new(),
                secret: Some("short".into()),
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn abuse_url_only_for_customer_destinations() {
        let slack = ChannelConfig::Slack(SlackConfig {
            webhook_url: "https://hooks.slack.com/x".into(),
        });
        assert_eq!(slack.abuse_url(), Some("https://hooks.slack.com/x"));
        let tg = ChannelConfig::Telegram(TelegramConfig {
            bot_token: "t".into(),
            chat_id: "1".into(),
        });
        assert_eq!(tg.abuse_url(), None);
    }

    #[test]
    fn whatsapp_redacts_token_and_validates_phone_shape() {
        let mut c = ChannelConfig::WhatsApp(WhatsAppConfig {
            access_token: "EAAGsecret".into(),
            phone_number_id: "106540352242922".into(),
            to: "+15551234567".into(),
            template_name: "uptime_alert".into(),
            language_code: None,
        });
        assert!(c.validate().is_ok());
        let r = c.redacted();
        assert_eq!(r["access_token"], "***");
        // Routing shape survives so the UI stays useful.
        assert_eq!(r["to"], "+15551234567");
        assert_eq!(r["template_name"], "uptime_alert");
        c.redact_in_place();
        assert!(c.has_redaction_sentinel());

        let bad = |f: fn(&mut WhatsAppConfig)| {
            let mut w = WhatsAppConfig {
                access_token: "tok".into(),
                phone_number_id: "123".into(),
                to: "15551234567".into(),
                template_name: "uptime_alert".into(),
                language_code: None,
            };
            f(&mut w);
            ChannelConfig::WhatsApp(w).validate()
        };
        assert!(bad(|w| w.access_token = "to k".into()).is_err());
        assert!(bad(|w| w.access_token = "tok\n".into()).is_err());
        assert!(bad(|w| w.phone_number_id = "not-numeric".into()).is_err());
        assert!(bad(|w| w.to = "abc".into()).is_err());
        assert!(bad(|w| w.to = "12345".into()).is_err());
        assert!(bad(|w| w.to = " 15551234567".into()).is_err());
        assert!(bad(|w| w.template_name = "Uptime Alert".into()).is_err());
        assert!(bad(|w| w.template_name = "".into()).is_err());
        assert!(bad(|w| w.language_code = Some("en US".into())).is_err());
    }

    #[test]
    fn mask_matches_canonical_redaction_sentinel() {
        // A redacted config round-tripped through the API must be detectable
        // as the sentinel (re-submitted "***" is rejected). That only holds
        // if this mask stays byte-equal to the canonical one.
        assert_eq!(MASK, crate::api::redaction::REDACTED);
    }

    #[test]
    fn name_validation_bounds() {
        assert!(validate_channel_name("  ").is_err());
        assert!(validate_channel_name(&"x".repeat(MAX_CHANNEL_NAME_LEN + 1)).is_err());
        assert!(validate_channel_name("Ops Slack").is_ok());
    }
}
