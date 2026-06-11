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

mod discord;
mod email;
mod google_chat;
mod msteams;
mod slack;
mod telegram;
mod telegram_app;
mod transport;
mod webhook;
mod whatsapp;

pub use discord::DiscordConfig;
pub use email::EmailConfig;
pub use google_chat::GoogleChatConfig;
pub use msteams::MsTeamsConfig;
pub use slack::SlackConfig;
pub use telegram::TelegramConfig;
pub use telegram_app::TelegramAppConfig;
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
    #[serde(rename = "telegram_app")]
    TelegramApp,
    #[serde(rename = "whatsapp")]
    WhatsApp,
    Discord,
    #[serde(rename = "msteams")]
    MsTeams,
    GoogleChat,
    Email,
}

impl ChannelKind {
    /// Every variant in declaration order. Used by the enum-drift integration
    /// test to compare against the live Postgres CHECK constraint on
    /// `notification_channels.kind`; keep in lockstep with the enum body.
    pub const ALL: &'static [Self] = &[
        Self::Webhook,
        Self::Slack,
        Self::Telegram,
        Self::TelegramApp,
        Self::WhatsApp,
        Self::Discord,
        Self::MsTeams,
        Self::GoogleChat,
        Self::Email,
    ];

    /// Stable string used in the Postgres `kind` CHECK constraint and the
    /// JSON wire form.
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Slack => "slack",
            Self::Telegram => "telegram",
            Self::TelegramApp => "telegram_app",
            Self::WhatsApp => "whatsapp",
            Self::Discord => "discord",
            Self::MsTeams => "msteams",
            Self::GoogleChat => "google_chat",
            Self::Email => "email",
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
    #[serde(rename = "telegram_app")]
    TelegramApp(TelegramAppConfig),
    #[serde(rename = "whatsapp")]
    WhatsApp(WhatsAppConfig),
    Discord(DiscordConfig),
    #[serde(rename = "msteams")]
    MsTeams(MsTeamsConfig),
    GoogleChat(GoogleChatConfig),
    Email(EmailConfig),
}

/// Apply `$body` to the inner [`TransportConfig`] of any variant. The one
/// place that has to enumerate the variants for delegation.
macro_rules! with_transport {
    ($self:expr, |$c:ident| $body:expr) => {
        match $self {
            ChannelConfig::Webhook($c) => $body,
            ChannelConfig::Slack($c) => $body,
            ChannelConfig::Telegram($c) => $body,
            ChannelConfig::TelegramApp($c) => $body,
            ChannelConfig::WhatsApp($c) => $body,
            ChannelConfig::Discord($c) => $body,
            ChannelConfig::MsTeams($c) => $body,
            ChannelConfig::GoogleChat($c) => $body,
            ChannelConfig::Email($c) => $body,
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

    /// See [`TransportConfig::operator_managed`].
    pub fn operator_managed(&self) -> bool {
        with_transport!(self, |c| c.operator_managed())
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
    /// Platform-disable note (e.g. the bot left the linked chat); `None`
    /// for operator-initiated disables, cleared on re-enable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// When the email address confirmed its verification link; reset on
    /// config change. Only consulted for `kind = email` — `None` elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Where this channel was last changed from (UI, API, or Terraform).
    pub write_source: WriteSource,
}

impl NotificationChannel {
    /// Email channels deliver only after the address confirms its
    /// verification link; every other kind is unaffected.
    pub fn awaiting_verification(&self) -> bool {
        self.kind == ChannelKind::Email && self.verified_at.is_none()
    }
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
    /// Routing id queryable without opening the sealed config.
    /// `serde(skip)`: only a transport's own flow may set it, never a
    /// request body.
    #[serde(skip)]
    #[schema(ignore)]
    pub external_ref: Option<String>,
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
            r#"{"type":"telegram_app","chat_id":"-100123"}"#,
            r#"{"type":"telegram_app","chat_id":"42","chat_title":"Ops"}"#,
            r#"{"type":"whatsapp","access_token":"tok","phone_number_id":"123","to":"15551234567","template_name":"uptime_alert"}"#,
            r#"{"type":"whatsapp","access_token":"tok","phone_number_id":"123","to":"15551234567","template_name":"uptime_alert","language_code":"en"}"#,
            r#"{"type":"discord","webhook_url":"https://discord.com/api/webhooks/1/x"}"#,
            r#"{"type":"msteams","webhook_url":"https://prod-77.westus.logic.azure.com/workflows/x"}"#,
            r#"{"type":"google_chat","webhook_url":"https://chat.googleapis.com/v1/spaces/A/messages?key=k&token=t"}"#,
            r#"{"type":"email","to":"ops@example.com"}"#,
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
            ChannelConfig::TelegramApp(TelegramAppConfig {
                chat_id: "1".into(),
                chat_title: None,
            }),
            ChannelConfig::WhatsApp(WhatsAppConfig {
                access_token: "tok".into(),
                phone_number_id: "123".into(),
                to: "15551234567".into(),
                template_name: "uptime_alert".into(),
                language_code: None,
            }),
            ChannelConfig::Discord(DiscordConfig {
                webhook_url: "https://discord.com/api/webhooks/1/x".into(),
            }),
            ChannelConfig::MsTeams(MsTeamsConfig {
                webhook_url: "https://prod-77.westus.logic.azure.com/workflows/x".into(),
            }),
            ChannelConfig::GoogleChat(GoogleChatConfig {
                webhook_url: "https://chat.googleapis.com/v1/spaces/A/messages".into(),
            }),
            ChannelConfig::Email(EmailConfig {
                to: "ops@example.com".into(),
            }),
        ];
        assert_eq!(configs.len(), ChannelKind::ALL.len());
        for c in configs {
            let tag = serde_json::to_value(&c).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(c.kind().as_db_str(), tag);
            // Exactly one transport is mintable only by the operator's flow.
            assert_eq!(
                c.operator_managed(),
                c.kind() == ChannelKind::TelegramApp,
                "operator_managed drifted for {tag}"
            );
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
    fn telegram_app_is_secretless_and_validates_chat_id() {
        let mut c = ChannelConfig::TelegramApp(TelegramAppConfig {
            chat_id: "-100123".into(),
            chat_title: Some("Ops".into()),
        });
        assert!(c.validate().is_ok());
        assert_eq!(c.abuse_url(), None);
        assert!(c.operator_managed());
        // Nothing to mask: the redacted copy is byte-identical and a
        // round-tripped config never reads as the sentinel.
        let r = c.redacted();
        assert_eq!(r["chat_id"], "-100123");
        assert_eq!(r["chat_title"], "Ops");
        c.redact_in_place();
        assert!(!c.has_redaction_sentinel());

        let bad = |chat_id: &str| {
            ChannelConfig::TelegramApp(TelegramAppConfig {
                chat_id: chat_id.into(),
                chat_title: None,
            })
            .validate()
        };
        assert!(bad("").is_err());
        assert!(bad("not-a-number").is_err());
        assert!(bad("@channelname").is_err());
    }

    #[test]
    fn provider_webhooks_pin_their_hosts() {
        let discord = |url: &str| {
            ChannelConfig::Discord(DiscordConfig {
                webhook_url: url.into(),
            })
            .validate()
        };
        assert!(discord("https://discord.com/api/webhooks/123/tok").is_ok());
        assert!(discord("https://ptb.discord.com/api/webhooks/123/tok").is_ok());
        assert!(discord("https://discordapp.com/api/webhooks/123/tok").is_ok());
        // Wrong provider, lookalike suffix, wrong path, plain http.
        assert!(discord("https://hooks.slack.com/services/T/B/x").is_err());
        assert!(discord("https://evildiscord.com/api/webhooks/1/x").is_err());
        assert!(discord("https://discord.com.evil.test/api/webhooks/1/x").is_err());
        assert!(discord("https://discord.com/webhooks/1/x").is_err());
        assert!(discord("http://discord.com/api/webhooks/1/x").is_err());

        let teams = |url: &str| {
            ChannelConfig::MsTeams(MsTeamsConfig {
                webhook_url: url.into(),
            })
            .validate()
        };
        assert!(teams("https://prod-77.westus.logic.azure.com/workflows/x/triggers/y").is_ok());
        assert!(teams("https://acme.api.powerplatform.com/workflows/x").is_ok());
        assert!(teams("https://logic.azure.com.evil.test/workflows/x").is_err());
        assert!(teams("https://example.com/workflows/x").is_err());

        let gchat = |url: &str| {
            ChannelConfig::GoogleChat(GoogleChatConfig {
                webhook_url: url.into(),
            })
            .validate()
        };
        assert!(gchat("https://chat.googleapis.com/v1/spaces/A/messages?key=k&token=t").is_ok());
        assert!(gchat("https://chat.googleapis.com./v1/spaces/A/messages").is_ok());
        assert!(gchat("https://googleapis.com/v1/spaces/A/messages").is_err());
        assert!(gchat("https://chat.example.com/v1/spaces/A/messages").is_err());

        // The mismatch message points at the escape hatch.
        let err = discord("https://example.com/hook").unwrap_err();
        assert!(err.contains("use the webhook type"), "{err}");

        // Whole-URL masking, same policy as slack.
        let mut c = ChannelConfig::Discord(DiscordConfig {
            webhook_url: "https://discord.com/api/webhooks/123/tok".into(),
        });
        assert_eq!(
            c.abuse_url(),
            Some("https://discord.com/api/webhooks/123/tok")
        );
        c.redact_in_place();
        assert!(c.has_redaction_sentinel());
        assert_eq!(c.redacted()["webhook_url"], MASK);
    }

    #[test]
    fn email_is_secretless_and_validates_address_shape() {
        let mut c = ChannelConfig::Email(EmailConfig {
            to: "ops+alerts@example.com".into(),
        });
        assert!(c.validate().is_ok());
        assert_eq!(c.abuse_url(), None);
        assert!(!c.operator_managed());
        // Nothing to mask: the address survives redaction and a round-trip
        // never reads as the sentinel.
        assert_eq!(c.redacted()["to"], "ops+alerts@example.com");
        c.redact_in_place();
        assert!(!c.has_redaction_sentinel());

        let bad = |to: &str| ChannelConfig::Email(EmailConfig { to: to.into() }).validate();
        assert!(bad("").is_err());
        assert!(bad("not-an-email").is_err());
        assert!(bad("a@b").is_err());
        assert!(bad("@example.com").is_err());
        assert!(bad("a b@example.com").is_err());
        assert!(bad("Ops@example.com").is_err());
        assert!(bad("a@.example.com").is_err());
        assert!(bad("a@example.com.").is_err());
        assert!(bad("a@example..com").is_err());
        assert!(bad(&format!("{}@example.com", "x".repeat(65))).is_err());
        assert!(bad(&format!("a@{}.com", "x".repeat(260))).is_err());
        // Role and tagged addresses are deliberately allowed.
        assert!(bad("ops@example.com").is_ok());
        assert!(bad("a+tag@sub.example.co").is_ok());
    }

    #[test]
    fn awaiting_verification_only_for_unverified_email() {
        use chrono::Utc;
        let ch = |kind_email: bool, verified: bool| NotificationChannel {
            id: Uuid::nil(),
            name: "n".into(),
            kind: if kind_email {
                ChannelKind::Email
            } else {
                ChannelKind::Slack
            },
            config: if kind_email {
                ChannelConfig::Email(EmailConfig {
                    to: "ops@example.com".into(),
                })
            } else {
                ChannelConfig::Slack(SlackConfig {
                    webhook_url: "https://hooks.slack.com/x".into(),
                })
            },
            enabled: true,
            disabled_reason: None,
            verified_at: verified.then(Utc::now),
            write_source: WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(ch(true, false).awaiting_verification());
        assert!(!ch(true, true).awaiting_verification());
        assert!(!ch(false, false).awaiting_verification());
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
