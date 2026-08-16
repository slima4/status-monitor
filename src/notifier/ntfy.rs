use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::domain::{IncidentUrgency, NotificationReason};
use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json_with_headers};
use crate::notifier::event::IncidentNotice;
use crate::notifier::{Notifier, truncate_bytes};

// ntfy's default server-side cap is 4096 BYTES; an over-limit publish is
// silently converted to a .txt attachment (or rejected when attachments
// are off), so the cap must count bytes, not chars.
const MAX_MESSAGE_BYTES: usize = 4096;

pub struct NtfyNotifier {
    client: OutboundHttpClient,
    /// Server root; JSON publishes POST here with the topic in the body.
    server_url: Url,
    topic: String,
    access_token: Option<String>,
}

#[derive(Serialize)]
struct Publish<'a> {
    topic: &'a str,
    title: &'a str,
    message: String,
    priority: u8,
    tags: [&'static str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    click: Option<&'a str>,
}

impl NtfyNotifier {
    pub fn new(
        client: OutboundHttpClient,
        server_url: Url,
        topic: String,
        access_token: Option<String>,
    ) -> Self {
        Self {
            client,
            server_url,
            topic,
            access_token,
        }
    }

    fn publish<'a>(topic: &'a str, notice: &'a IncidentNotice) -> Publish<'a> {
        let resolved = notice.reason == NotificationReason::Resolved;
        Publish {
            topic,
            title: notice.label(),
            message: truncate_bytes(&notice.plain_text(), MAX_MESSAGE_BYTES),
            priority: match (resolved, notice.urgency) {
                (true, _) => 3,
                (false, IncidentUrgency::High) => 4,
                (false, IncidentUrgency::Low) => 3,
            },
            tags: if resolved {
                ["white_check_mark"]
            } else {
                ["rotating_light"]
            },
            click: notice.url.as_deref(),
        }
    }
}

#[async_trait]
impl Notifier for NtfyNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let mut headers = BTreeMap::new();
        if let Some(token) = &self.access_token {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        post_json_with_headers(
            &self.client,
            &self.server_url,
            &Self::publish(&self.topic, notice),
            &headers,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::domain::IncidentSeverity;

    fn notice(reason: NotificationReason, urgency: IncidentUrgency) -> IncidentNotice {
        IncidentNotice {
            incident_id: Uuid::from_u128(7),
            reason,
            monitor_name: Some("api-prod".into()),
            title: None,
            severity: IncidentSeverity::Major,
            urgency,
            started_at: Utc.with_ymd_and_hms(2026, 6, 12, 8, 0, 0).unwrap(),
            ended_at: None,
            error_sample: None,
            regions_down: Vec::new(),
            regions_up: Vec::new(),
            url: Some("https://app.uptimepage.dev/i/7".into()),
            note: None,
        }
    }

    #[test]
    fn open_publish_matches_wire_shape() {
        let n = notice(NotificationReason::Opened, IncidentUrgency::High);
        let v = serde_json::to_value(NtfyNotifier::publish("ops", &n)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "topic": "ops",
                "title": "api-prod",
                "message": "api-prod — major incident OPEN\nhttps://app.uptimepage.dev/i/7",
                "priority": 4,
                "tags": ["rotating_light"],
                "click": "https://app.uptimepage.dev/i/7"
            })
        );
    }

    #[test]
    fn priorities_and_tags_follow_lifecycle() {
        let low = notice(NotificationReason::Opened, IncidentUrgency::Low);
        assert_eq!(NtfyNotifier::publish("t", &low).priority, 3);

        let resolved = notice(NotificationReason::Resolved, IncidentUrgency::High);
        let p = NtfyNotifier::publish("t", &resolved);
        assert_eq!(p.priority, 3);
        assert_eq!(p.tags, ["white_check_mark"]);

        let mut no_url = notice(NotificationReason::Opened, IncidentUrgency::High);
        no_url.url = None;
        let v = serde_json::to_value(NtfyNotifier::publish("t", &no_url)).unwrap();
        assert!(v.get("click").is_none());
    }
}
