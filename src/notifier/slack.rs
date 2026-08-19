use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::domain::NotificationReason;
use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;

pub struct SlackNotifier {
    client: OutboundHttpClient,
    webhook_url: Url,
}

#[derive(Serialize)]
struct SlackPayload<'a> {
    text: &'a str,
}

impl SlackNotifier {
    pub fn new(client: OutboundHttpClient, webhook_url: Url) -> Self {
        Self {
            client,
            webhook_url,
        }
    }

    fn render_incident(n: &IncidentNotice) -> String {
        match &n.note {
            Some(note) => format!("{}\n{}", Self::incident_line(n), mrkdwn_escape(note)),
            None => Self::incident_line(n),
        }
    }

    /// Slack builds its own text rather than using
    /// [`IncidentNotice::plain_text`], so anything added to the shared renderer
    /// has to be added here too.
    fn incident_line(n: &IncidentNotice) -> String {
        let link = n
            .url
            .as_deref()
            .map(|u| format!(" <{u}|view incident>"))
            .unwrap_or_default();
        // Customer-supplied monitor name + error text must not inject Slack
        // markup (live `<url|text>` links, `@channel` mentions) into the
        // responders' channel.
        let label = mrkdwn_escape(n.label());
        match n.reason {
            NotificationReason::Opened
            | NotificationReason::Escalated
            | NotificationReason::Reopened => format!(
                "*{label}* — {sev} incident {state}{err}{regions}{link}",
                sev = n.severity.as_db_str(),
                state = n.open_state(),
                err = n
                    .error_sample
                    .as_deref()
                    .map(|e| format!(": {}", mrkdwn_escape(e)))
                    .unwrap_or_default(),
                regions = region_line(n),
            ),
            NotificationReason::Resolved => {
                let dur = n
                    .duration_minutes()
                    .map(|m| format!(" after {m}m"))
                    .unwrap_or_default();
                format!("*{label}* — incident RESOLVED{dur}{link}")
            }
            NotificationReason::NoData => {
                format!("*{label}* — NO DATA: monitoring interrupted{link}")
            }
            NotificationReason::DataResumed => format!("*{label}* — monitoring RESUMED{link}"),
        }
    }
}

/// Escape the three characters Slack mrkdwn treats specially, so customer text
/// renders literally rather than as live links or control sequences.
fn mrkdwn_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Per-region breakdown line, escaped for mrkdwn; empty for single-region.
fn region_line(n: &IncidentNotice) -> String {
    n.region_summary(mrkdwn_escape, " • ")
        .map(|s| format!("\n• {s}"))
        .unwrap_or_default()
}

#[async_trait]
impl Notifier for SlackNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let text = Self::render_incident(notice);
        post_json(
            &self.client,
            &self.webhook_url,
            &SlackPayload { text: &text },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{IncidentOrigin, IncidentSeverity, IncidentUrgency};
    use chrono::Utc;
    use uuid::Uuid;

    fn notice(reason: NotificationReason) -> IncidentNotice {
        IncidentNotice {
            incident_id: Uuid::from_u128(7),
            reason,
            monitor_name: Some("api".into()),
            title: None,
            severity: IncidentSeverity::Major,
            urgency: IncidentUrgency::High,
            origin: IncidentOrigin::Monitor,
            started_at: Utc::now(),
            ended_at: None,
            error_sample: None,
            regions_down: Vec::new(),
            regions_up: Vec::new(),
            url: None,
            note: None,
        }
    }

    /// Slack does not use the shared body renderer, so a note added there
    /// reaches it only because this transport appends it too.
    #[test]
    fn a_note_reaches_slack_even_though_it_renders_its_own_body() {
        let mut n = notice(NotificationReason::Opened);
        n.note = Some("Flapping: alerts held".into());
        assert!(SlackNotifier::render_incident(&n).contains("Flapping: alerts held"));
    }
}
