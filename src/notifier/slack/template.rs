//! Predefined Block Kit layouts, one per kind of news. The reason picks the
//! template, so a new kind of event adds a variant instead of another branch
//! inside one renderer.

use chrono::{DateTime, Utc};

use crate::domain::{IncidentOrigin, IncidentSeverity, NotificationReason};
use crate::notifier::event::IncidentNotice;
use crate::text::truncate_chars;

use super::blocks::{Block, SECTION_MAX, Text, escape, plain_label};
use super::pings_someone;

/// Error text is customer-controlled and unbounded in storage; the section it
/// lands in has a hard cap, and Slack refuses the message rather than the block.
pub const MAX_ERROR_CHARS: usize = 500;

/// Escaping can quintuple the text (`&` becomes `&amp;`), so the cap above holds
/// against Slack's only while the worst case plus the fence still fits.
const _: () = assert!(MAX_ERROR_CHARS * 5 + 32 <= SECTION_MAX);

#[derive(Debug, Clone, Copy)]
pub enum SlackTemplate {
    Incident,
    Resolved,
    NoData,
    DataResumed,
}

impl SlackTemplate {
    pub fn for_reason(reason: NotificationReason) -> Self {
        match reason {
            NotificationReason::Opened
            | NotificationReason::Reopened
            | NotificationReason::Escalated => Self::Incident,
            NotificationReason::Resolved => Self::Resolved,
            NotificationReason::NoData => Self::NoData,
            NotificationReason::DataResumed => Self::DataResumed,
        }
    }

    /// The ping is filtered here rather than by the caller, so no future call
    /// site can wake a whole channel with an all-clear.
    pub fn render(self, n: &IncidentNotice, mention: Option<&str>) -> Vec<Block> {
        let mention = mention.filter(|_| pings_someone(n.reason));
        let mut blocks = vec![
            Block::header(&format!("{} {}", self.icon(n), plain_label(n.label()))),
            Block::section(lead(mention, self.headline(n))),
        ];
        blocks.extend(self.detail(n));
        if let Some(note) = &n.note {
            blocks.push(Block::context(escape(note)));
        }
        if let Some(url) = n.url.as_deref().filter(|u| is_web_link(u)) {
            blocks.push(Block::link_button("View incident", url));
        }
        blocks
    }

    fn icon(self, n: &IncidentNotice) -> &'static str {
        match self {
            Self::Incident => match n.severity {
                IncidentSeverity::Critical => "🚨",
                IncidentSeverity::Major => "🔴",
                IncidentSeverity::Minor => "🟠",
            },
            Self::Resolved => "✅",
            Self::NoData => "⚠️",
            Self::DataResumed => "🟢",
        }
    }

    fn headline(self, n: &IncidentNotice) -> String {
        match self {
            Self::Incident => format!(
                "*{sev} incident {state}*",
                sev = n.severity.as_db_str(),
                state = n.open_state(),
            ),
            Self::Resolved => "*Incident resolved*".into(),
            Self::NoData => "*No data*: monitoring interrupted, no check results received".into(),
            Self::DataResumed => "*Monitoring resumed*, receiving check results again".into(),
        }
    }

    fn detail(self, n: &IncidentNotice) -> Vec<Block> {
        match self {
            Self::Incident => {
                let mut fields = vec![Text::field("Started", &at(n.started_at))];
                if let Some(regions) = n.region_summary(escape, " · ") {
                    fields.push(Text::field("Regions", &regions));
                }
                // No failing check behind a declared incident, so the card must
                // not read as a detection the product cannot support.
                if n.origin == IncidentOrigin::Manual {
                    fields.push(Text::field(
                        "Origin",
                        "declared by hand, no monitor detection",
                    ));
                }
                let mut out: Vec<Block> = Block::fields(fields).into_iter().collect();
                if let Some(err) = n.error_sample.as_deref() {
                    out.push(Block::section(format!("*Error*\n```{}```", fenced(err))));
                }
                out
            }
            Self::Resolved => {
                let mut fields = vec![Text::field("Started", &at(n.started_at))];
                if let Some(end) = n.ended_at {
                    fields.push(Text::field("Resolved", &at(end)));
                }
                if let Some(minutes) = n.duration_minutes() {
                    fields.push(Text::field("Duration", &spell_duration(minutes)));
                }
                Block::fields(fields).into_iter().collect()
            }
            Self::NoData => Block::fields(vec![Text::field("Since", &at(n.started_at))])
                .into_iter()
                .collect(),
            Self::DataResumed => Vec::new(),
        }
    }
}

fn lead(mention: Option<&str>, headline: String) -> String {
    match mention {
        Some(m) => format!("{m} {headline}"),
        None => headline,
    }
}

/// Slack renders this in each reader's own timezone, so an on-call in another
/// country does not convert UTC by hand at 3am.
fn at(ts: DateTime<Utc>) -> String {
    format!(
        "<!date^{epoch}^{{date_short_pretty}} {{time}}|{fallback}>",
        epoch = ts.timestamp(),
        fallback = ts.format("%Y-%m-%d %H:%M UTC"),
    )
}

/// Error text for a code fence. Backticks in the payload would close the fence
/// early and hand the rest of the error to the mrkdwn parser.
fn fenced(error: &str) -> String {
    escape(&truncate_chars(error, MAX_ERROR_CHARS)).replace('`', "'")
}

/// A `url` Slack refuses takes the whole message with it, so a base URL set
/// without a scheme costs the button, never the alert.
fn is_web_link(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

fn spell_duration(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::IncidentUrgency;
    use serde_json::Value;
    use uuid::Uuid;

    fn notice(reason: NotificationReason) -> IncidentNotice {
        IncidentNotice {
            incident_id: Uuid::from_u128(7),
            reason,
            monitor_name: Some("api-prod".into()),
            title: None,
            severity: IncidentSeverity::Major,
            urgency: IncidentUrgency::High,
            origin: IncidentOrigin::Monitor,
            started_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            ended_at: None,
            error_sample: Some("HTTP 500".into()),
            regions_down: Vec::new(),
            regions_up: Vec::new(),
            url: Some("https://app.test/i/7".into()),
            note: None,
        }
    }

    fn render(n: &IncidentNotice, mention: Option<&str>) -> Vec<Block> {
        SlackTemplate::for_reason(n.reason).render(n, mention)
    }

    fn json(blocks: Vec<Block>) -> String {
        serde_json::to_string(&blocks).unwrap()
    }

    /// The longest rendered text object in characters: Slack caps each one and
    /// refuses the whole message when any is over.
    fn longest_text(blocks: Vec<Block>) -> usize {
        fn walk(v: &Value, worst: &mut usize) {
            match v {
                Value::Object(map) => {
                    if let Some(Value::String(s)) = map.get("text") {
                        *worst = (*worst).max(s.chars().count());
                    }
                    map.values().for_each(|v| walk(v, worst));
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, worst)),
                _ => {}
            }
        }
        let mut worst = 0;
        walk(&serde_json::to_value(blocks).unwrap(), &mut worst);
        worst
    }

    #[test]
    fn an_open_incident_leads_with_the_ping_and_links_the_incident() {
        let blocks = json(render(&notice(NotificationReason::Opened), Some("<!here>")));
        assert!(blocks.contains(r#""text":"🔴 api-prod""#), "{blocks}");
        assert!(
            blocks.contains(r#"<!here> *major incident OPEN*"#),
            "{blocks}"
        );
        assert!(blocks.contains("*Error*\\n```HTTP 500```"), "{blocks}");
        assert!(
            blocks.contains(r#""url":"https://app.test/i/7""#),
            "{blocks}"
        );
    }

    #[test]
    fn a_resolved_incident_reports_how_long_it_ran() {
        let mut n = notice(NotificationReason::Resolved);
        n.ended_at = Some(n.started_at + chrono::Duration::minutes(95));
        let blocks = json(render(&n, None));
        assert!(blocks.contains("*Duration*\\n1h 35m"), "{blocks}");
        assert!(blocks.contains("✅ api-prod"), "{blocks}");
    }

    /// The customer names the monitor and the error, and both land in a channel
    /// where `<!channel>` would wake everybody.
    #[test]
    fn customer_text_cannot_smuggle_a_ping_into_the_message() {
        let mut n = notice(NotificationReason::Opened);
        n.monitor_name = Some("<!channel> api".into());
        n.error_sample = Some("<!here> fix me".into());
        let blocks = json(render(&n, None));
        assert!(!blocks.contains("<!channel>"), "{blocks}");
        assert!(!blocks.contains("<!here>"), "{blocks}");
    }

    /// Backticks in the error would close the fence early and hand the rest of
    /// the customer's text to the mrkdwn parser.
    #[test]
    fn an_error_cannot_break_out_of_its_code_fence() {
        let mut n = notice(NotificationReason::Opened);
        n.error_sample = Some("``` *not bold* ```".into());
        assert_eq!(json(render(&n, None)).matches("```").count(), 2);
    }

    #[test]
    fn a_multi_region_incident_names_the_regions() {
        let mut n = notice(NotificationReason::Opened);
        n.regions_down = vec!["eu-helsinki".into()];
        n.regions_up = vec!["apac-sg".into()];
        let blocks = json(render(&n, None));
        assert!(blocks.contains("*Regions*"), "{blocks}");
        assert!(
            blocks.contains("down: eu-helsinki · up: apac-sg"),
            "{blocks}"
        );
    }

    #[test]
    fn a_single_region_incident_omits_the_breakdown() {
        let mut n = notice(NotificationReason::Opened);
        n.regions_down = vec!["eu-helsinki".into()];
        assert!(!json(render(&n, None)).contains("*Regions*"));
    }

    /// A declared incident that reads like a detection makes the product look
    /// as if it misfired, and only an open one still describes the present.
    #[test]
    fn a_declared_incident_says_so_instead_of_claiming_a_detection() {
        let mut n = notice(NotificationReason::Opened);
        n.origin = IncidentOrigin::Manual;
        assert!(
            json(render(&n, None)).contains("declared by hand, no monitor detection"),
            "an open incident owns up to being declared"
        );

        n.reason = NotificationReason::Resolved;
        assert!(
            !json(render(&n, None)).contains("declared by hand"),
            "a closed one does not relitigate where it came from"
        );
    }

    /// An all-clear that pinged would wake the room the alert spared.
    #[test]
    fn only_the_events_that_need_a_human_carry_the_ping() {
        for reason in [
            NotificationReason::Opened,
            NotificationReason::Reopened,
            NotificationReason::Escalated,
            NotificationReason::NoData,
        ] {
            assert!(
                json(render(&notice(reason), Some("<!here>"))).contains("<!here>"),
                "{reason:?}"
            );
        }
        for reason in [
            NotificationReason::Resolved,
            NotificationReason::DataResumed,
        ] {
            assert!(
                !json(render(&notice(reason), Some("<!here>"))).contains("<!here>"),
                "{reason:?}"
            );
        }
    }

    /// A base URL set without a scheme is not a link Slack accepts, and it
    /// refuses the message rather than the button.
    #[test]
    fn an_unusable_link_costs_the_button_not_the_alert() {
        let mut n = notice(NotificationReason::Opened);
        n.url = Some("app.example.test/incidents/7".into());
        let blocks = json(render(&n, None));
        assert!(!blocks.contains("actions"), "{blocks}");
        assert!(blocks.contains("major incident OPEN"), "{blocks}");
    }

    /// A page of HTML in the error field would push its section past Slack's
    /// cap, and Slack refuses the whole message rather than trimming it.
    #[test]
    fn a_huge_error_cannot_cost_the_alert() {
        let mut n = notice(NotificationReason::Opened);
        n.error_sample = Some("&".repeat(20_000));
        assert!(longest_text(render(&n, None)) <= SECTION_MAX);
    }
}
