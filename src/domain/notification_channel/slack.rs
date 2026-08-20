use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ChannelKind;
use super::transport::{MASK, TransportConfig, require_https};

/// Enough to page a group plus a fallback, few enough that a paste of the
/// whole member list can't turn one alert into a workspace-wide ping.
const MAX_MENTIONS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SlackConfig {
    /// Incoming-webhook URL. The path carries the workspace token, so the
    /// whole value is treated as a secret.
    pub webhook_url: String,
    /// Who to ping on an alert: `@here`, `@channel`, a user-group id (`S…`)
    /// or a member id (`U…`), space or comma separated. Visible routing, not
    /// a secret, so it survives redaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention: Option<String>,
}

impl SlackConfig {
    /// A copy without the workspace-wide pings: a config test should prove
    /// the routing, not wake everybody in the channel.
    pub fn without_broadcast_mention(&self) -> Self {
        let mention = self
            .mention
            .as_deref()
            .map(|raw| {
                mention_tokens(raw)
                    .filter(|t| !is_broadcast(t))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|m| !m.is_empty());
        Self {
            webhook_url: self.webhook_url.clone(),
            mention,
        }
    }

    pub fn mention_markup(&self) -> Option<String> {
        let raw = self.mention.as_deref()?;
        let markup = mention_tokens(raw)
            .filter_map(token_markup)
            .collect::<Vec<_>>()
            .join(" ");
        (!markup.is_empty()).then_some(markup)
    }
}

fn mention_tokens(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', ' ', '\t', '\n', '\r'])
        .filter(|t| !t.is_empty())
}

/// `@here` / `@channel`: everyone in the channel, not a named responder.
fn is_broadcast(token: &str) -> bool {
    let t = token.strip_prefix('@').unwrap_or(token);
    t.eq_ignore_ascii_case("here") || t.eq_ignore_ascii_case("channel")
}

/// One token as Slack markup; `None` where Slack would render inert text.
fn token_markup(token: &str) -> Option<String> {
    let t = token.strip_prefix('@').unwrap_or(token);
    // Slack ids are uppercase alphanumeric and at least 9 long. The floor is
    // what separates an id from a shouted handle like `@SRE`, which would
    // otherwise render as a ping that silently reaches nobody.
    let id_like = |s: &str| {
        (9..=24).contains(&s.len())
            && s.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    if t.eq_ignore_ascii_case("here") {
        Some("<!here>".into())
    } else if t.eq_ignore_ascii_case("channel") {
        Some("<!channel>".into())
    } else if t.starts_with('S') && id_like(t) {
        Some(format!("<!subteam^{t}>"))
    } else if (t.starts_with('U') || t.starts_with('W')) && id_like(t) {
        Some(format!("<@{t}>"))
    } else {
        None
    }
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
        require_https(&self.webhook_url, "webhook_url")?;
        if let Some(raw) = &self.mention {
            let tokens: Vec<&str> = mention_tokens(raw).collect();
            if tokens.is_empty() {
                return Err("mention is blank — leave it unset to ping nobody".into());
            }
            if tokens.len() > MAX_MENTIONS {
                return Err(format!("mention takes at most {MAX_MENTIONS} entries"));
            }
            for t in tokens {
                if token_markup(t).is_none() {
                    return Err(format!(
                        "Slack cannot ping \"{t}\" — use @here, @channel, a user-group id (S…) \
                         or a member id (U… / W… on Enterprise Grid)"
                    ));
                }
            }
        }
        Ok(())
    }

    fn abuse_url(&self) -> Option<&str> {
        Some(&self.webhook_url)
    }

    fn operator_managed(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mention: Option<&str>) -> SlackConfig {
        SlackConfig {
            webhook_url: "https://hooks.slack.com/services/T/B/x".into(),
            mention: mention.map(str::to_string),
        }
    }

    #[test]
    fn every_accepted_mention_form_renders_as_a_real_ping() {
        assert_eq!(cfg(Some("@here")).mention_markup().unwrap(), "<!here>");
        assert_eq!(cfg(Some("channel")).mention_markup().unwrap(), "<!channel>");
        assert_eq!(
            cfg(Some("S01ABC234")).mention_markup().unwrap(),
            "<!subteam^S01ABC234>"
        );
        assert_eq!(
            cfg(Some("U01ABC234")).mention_markup().unwrap(),
            "<@U01ABC234>"
        );
        assert_eq!(
            cfg(Some("@sre, S01ABC234")).mention_markup(),
            Some("<!subteam^S01ABC234>".into()),
            "an unpingable handle drops out instead of printing as dead text"
        );
    }

    #[test]
    fn a_handle_without_its_id_is_rejected_because_slack_would_not_ping_it() {
        assert!(cfg(Some("@sre")).validate().is_err());
        // Shouted, so it looks like an id until Slack renders it as dead text.
        assert!(cfg(Some("@SRE")).validate().is_err());
        assert!(cfg(Some("@DBTEAM")).validate().is_err());
        assert!(cfg(Some("<!subteam^S01ABC234>")).validate().is_err());
        assert!(cfg(Some("  ")).validate().is_err());
        assert!(
            cfg(Some("here channel S01ABC234 U01ABC234 U05XYZ678 U09QRS012"))
                .validate()
                .is_err()
        );
        assert!(cfg(Some("@here S01ABC234")).validate().is_ok());
        assert!(cfg(None).validate().is_ok());
    }

    #[test]
    fn a_test_send_keeps_the_group_ping_and_drops_the_broadcast() {
        let quiet = cfg(Some("@here S01ABC234, channel")).without_broadcast_mention();
        assert_eq!(quiet.mention_markup().unwrap(), "<!subteam^S01ABC234>");
        assert_eq!(
            cfg(Some("@channel")).without_broadcast_mention().mention,
            None
        );
        assert_eq!(cfg(None).without_broadcast_mention().mention, None);
    }

    #[test]
    fn a_mention_survives_redaction_because_it_is_not_a_secret() {
        let mut c = cfg(Some("@here"));
        c.redact_in_place();
        assert_eq!(c.mention.as_deref(), Some("@here"));
        assert!(c.has_redaction_sentinel());
    }
}
