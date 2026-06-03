use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::{AlertEvent, AlertKind, IncidentNotice};
use crate::domain::NotificationReason;

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

    fn render(event: &AlertEvent) -> String {
        match event.kind {
            AlertKind::Down => format!(
                ":rotating_light: *{name}* is DOWN ({failures} consecutive failures, status={status}{err})",
                name = event.target_name,
                failures = event.consecutive_failures,
                status = event.last_status.as_str(),
                err = event
                    .last_error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            ),
            AlertKind::Recovered => format!(
                ":white_check_mark: *{name}* has RECOVERED",
                name = event.target_name
            ),
        }
    }

    fn render_incident(n: &IncidentNotice) -> String {
        let link = n
            .url
            .as_deref()
            .map(|u| format!(" <{u}|view incident>"))
            .unwrap_or_default();
        match n.reason {
            NotificationReason::Opened | NotificationReason::Escalated => format!(
                "*{label}* — {sev} incident OPEN{err}{link}",
                label = n.label(),
                sev = n.severity.as_db_str(),
                err = n
                    .error_sample
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default(),
            ),
            NotificationReason::Reopened => {
                format!("*{label}* — incident REOPENED{link}", label = n.label())
            }
            NotificationReason::Resolved => {
                let dur = n
                    .duration_minutes()
                    .map(|m| format!(" after {m}m"))
                    .unwrap_or_default();
                format!("*{label}* — incident RESOLVED{dur}{link}", label = n.label())
            }
        }
    }
}

#[async_trait]
impl Notifier for SlackNotifier {
    async fn notify(&self, event: &AlertEvent) -> Result<()> {
        let text = Self::render(event);
        post_json(
            &self.client,
            &self.webhook_url,
            &SlackPayload { text: &text },
        )
        .await
    }

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
