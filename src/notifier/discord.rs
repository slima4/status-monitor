//! Discord's dialect of the alert card: an embed with a coloured bar, inline
//! fields and Discord's own timestamp markup. Limits are applied per field,
//! because Discord refuses the whole message rather than trimming an embed.

use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::card::{AlertCard, CardField, CardTone, CardValue, escape_markdown};
use crate::notifier::event::IncidentNotice;
use crate::notifier::truncate_chars;

const TITLE_MAX: usize = 256;
const DESCRIPTION_MAX: usize = 2048;
const FIELD_NAME_MAX: usize = 32;
const FIELD_VALUE_MAX: usize = 512;
const MAX_FIELDS: usize = 6;
/// Discord counts the whole embed, not each field, and refuses the message over
/// it. The per-field caps above are only safe while their worst case fits here.
const EMBED_MAX: usize = 6000;
const _: () = assert!(
    TITLE_MAX + DESCRIPTION_MAX + MAX_FIELDS * (FIELD_NAME_MAX + FIELD_VALUE_MAX) <= EMBED_MAX
);

pub struct DiscordNotifier {
    client: OutboundHttpClient,
    webhook_url: Url,
}

#[derive(Serialize)]
struct DiscordPayload {
    embeds: [Embed; 1],
}

#[derive(Serialize)]
struct Embed {
    title: String,
    description: String,
    color: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fields: Vec<EmbedField>,
}

#[derive(Serialize)]
struct EmbedField {
    name: String,
    value: String,
    inline: bool,
}

impl DiscordNotifier {
    pub fn new(client: OutboundHttpClient, mut webhook_url: Url) -> Self {
        // Without `wait=true` Discord answers 204 before delivering, hiding
        // failures from the retry loop.
        webhook_url.query_pairs_mut().append_pair("wait", "true");
        Self {
            client,
            webhook_url,
        }
    }

    fn embed(card: &AlertCard) -> Embed {
        let mut description = format!("**{}**", escape(&card.headline));
        if let Some(error) = &card.error {
            description.push_str(&format!("\n```{}```", fenced(error)));
        }
        if let Some(note) = &card.note {
            description.push_str(&format!("\n{}", escape(note)));
        }
        Embed {
            title: truncate_chars(&escape(&card.heading()), TITLE_MAX),
            description: truncate_chars(&description, DESCRIPTION_MAX),
            color: color(card.tone),
            url: card.link.clone(),
            fields: card.fields.iter().take(MAX_FIELDS).map(field).collect(),
        }
    }
}

fn field(f: &CardField) -> EmbedField {
    let value = match &f.value {
        CardValue::Text(text) => escape(text),
        // Rendered in the reader's own timezone by their client.
        CardValue::Time(at) => format!("<t:{}:f>", at.timestamp()),
    };
    EmbedField {
        name: truncate_chars(f.label, FIELD_NAME_MAX),
        value: truncate_chars(&value, FIELD_VALUE_MAX),
        inline: true,
    }
}

/// Loudest first: the bar has to descend with severity, or the quieter
/// incident is the one that catches the eye.
fn color(tone: CardTone) -> u32 {
    match tone {
        CardTone::Critical => 0xED_42_45,
        CardTone::Major => 0xE6_7E_22,
        CardTone::Minor => 0xF1_C4_0F,
        CardTone::Warning => 0x95_A5_A6,
        CardTone::Recovered | CardTone::Resumed => 0x2E_CC_71,
    }
}

/// Customer text renders literally rather than as emphasis or a live link.
/// Mentions need no handling: Discord does not resolve them inside an embed.
fn escape(s: &str) -> String {
    escape_markdown(s, &['\\', '`', '*', '_', '~', '|', '[', ']', '<', '>'])
}

/// Error text for a code fence. Backticks in the payload would close the fence
/// early and hand the rest of the error to the markdown parser.
fn fenced(error: &str) -> String {
    error.replace('`', "'")
}

#[async_trait]
impl Notifier for DiscordNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let embed = Self::embed(&AlertCard::for_notice(notice));
        post_json(
            &self.client,
            &self.webhook_url,
            &DiscordPayload { embeds: [embed] },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NotificationReason;
    use crate::notifier::card::tests::notice;

    fn notifier(url: &str) -> DiscordNotifier {
        DiscordNotifier::new(
            crate::http_outbound::build_outbound_client(
                crate::security::SsrfGuard::relaxed_for_tests(),
            ),
            url.parse().unwrap(),
        )
    }

    fn embed(n: &IncidentNotice) -> serde_json::Value {
        serde_json::to_value(DiscordNotifier::embed(&AlertCard::for_notice(n))).unwrap()
    }

    #[test]
    fn send_url_carries_wait_for_synchronous_errors() {
        let n = notifier("https://discord.com/api/webhooks/123/tok");
        assert_eq!(
            n.webhook_url.as_str(),
            "https://discord.com/api/webhooks/123/tok?wait=true"
        );
        let threaded = notifier("https://discord.com/api/webhooks/123/tok?thread_id=42");
        assert_eq!(
            threaded.webhook_url.as_str(),
            "https://discord.com/api/webhooks/123/tok?thread_id=42&wait=true"
        );
    }

    #[test]
    fn an_open_incident_becomes_a_coloured_embed() {
        let v = embed(&notice(NotificationReason::Opened));
        assert_eq!(v["title"], "🔴 api-prod");
        assert_eq!(v["color"], 0xE6_7E_22);
        assert_eq!(v["url"], "https://app.test/i/7");
        assert!(
            v["description"]
                .as_str()
                .unwrap()
                .starts_with("**major incident OPEN**"),
            "{v}"
        );
        assert_eq!(v["fields"][0]["name"], "Started");
        assert_eq!(v["fields"][0]["value"], "<t:1700000000:f>");
    }

    #[test]
    fn a_resolved_incident_turns_the_bar_green() {
        let mut n = notice(NotificationReason::Resolved);
        n.ended_at = Some(n.started_at + chrono::Duration::minutes(95));
        let v = embed(&n);
        assert_eq!(v["color"], 0x2E_CC_71);
        assert_eq!(v["fields"][2]["value"], "1h 35m");
    }

    /// Customer text must not turn into emphasis, a link, or a broken fence.
    #[test]
    fn customer_text_renders_literally() {
        let mut n = notice(NotificationReason::Opened);
        n.error_sample = Some("``` **boom** ```".into());
        let v = embed(&n);
        let description = v["description"].as_str().unwrap();
        assert_eq!(description.matches("```").count(), 2, "{description}");
        assert!(description.contains("'''"), "{description}");
    }

    /// A monitor whose name is a page of text would push the embed past
    /// Discord's caps, and Discord refuses the message outright.
    #[test]
    fn an_over_long_field_is_cut_to_discords_cap() {
        let mut n = notice(NotificationReason::Opened);
        n.monitor_name = Some("x".repeat(5_000));
        n.note = Some("y".repeat(5_000));
        let v = embed(&n);
        assert!(v["title"].as_str().unwrap().chars().count() <= TITLE_MAX);
        assert!(v["description"].as_str().unwrap().chars().count() <= DESCRIPTION_MAX);
    }

    /// The embed title renders markdown too, so a monitor name cannot format
    /// itself into the loudest line on the card.
    #[test]
    fn a_monitor_name_cannot_format_the_title() {
        let mut n = notice(NotificationReason::Opened);
        n.monitor_name = Some("**PROD** ~~down~~".into());
        assert_eq!(embed(&n)["title"], r"🔴 \*\*PROD\*\* \~\~down\~\~");
    }
}
