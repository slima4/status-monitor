use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::domain::{IncidentUrgency, NotificationReason};
use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::event::IncidentNotice;
use crate::notifier::{Notifier, truncate_chars};

const MESSAGES_URL: &str = "https://api.pushover.net/1/messages.json";
// Pushover caps: message 1024, title 250, url 512 characters.
const MAX_MESSAGE_CHARS: usize = 1024;
const MAX_TITLE_CHARS: usize = 250;
const MAX_URL_CHARS: usize = 512;

pub struct PushoverNotifier {
    client: OutboundHttpClient,
    token: String,
    user: String,
    device: Option<String>,
}

#[derive(Serialize)]
struct Message<'a> {
    token: &'a str,
    user: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<&'a str>,
    title: String,
    message: String,
    priority: i8,
    timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url_title: Option<&'static str>,
}

impl PushoverNotifier {
    pub fn new(
        client: OutboundHttpClient,
        token: String,
        user: String,
        device: Option<String>,
    ) -> Self {
        Self {
            client,
            token,
            user,
            device,
        }
    }

    fn message<'a>(
        token: &'a str,
        user: &'a str,
        device: Option<&'a str>,
        notice: &'a IncidentNotice,
    ) -> Message<'a> {
        let url = notice
            .url
            .as_deref()
            .map(|u| truncate_chars(u, MAX_URL_CHARS));
        Message {
            token,
            user,
            device,
            title: truncate_chars(notice.label(), MAX_TITLE_CHARS),
            message: truncate_chars(&notice.plain_text(), MAX_MESSAGE_CHARS),
            // Emergency (2) is deliberately out: it mandates retry/expire
            // plus a receipt lifecycle. 1 bypasses quiet hours; resolves go
            // quiet at -1.
            priority: match (notice.reason, notice.urgency) {
                (NotificationReason::Resolved, _) => -1,
                (_, IncidentUrgency::High) => 1,
                (_, IncidentUrgency::Low) => 0,
            },
            // Pushover renders the push at this time — a resolve is news
            // from the resolve moment, not the open.
            timestamp: match notice.reason {
                NotificationReason::Resolved => notice.ended_at.unwrap_or(notice.started_at),
                _ => notice.started_at,
            }
            .timestamp(),
            url_title: url.is_some().then_some("Open incident"),
            url,
        }
    }
}

#[async_trait]
impl Notifier for PushoverNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let url: Url = MESSAGES_URL.parse().expect("static messages URL parses");
        let msg = Self::message(&self.token, &self.user, self.device.as_deref(), notice);
        post_json(&self.client, &url, &msg).await
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
        }
    }

    #[test]
    fn message_matches_wire_shape() {
        let n = notice(NotificationReason::Opened, IncidentUrgency::High);
        let v = serde_json::to_value(PushoverNotifier::message(
            "apptoken",
            "userkey",
            Some("droid2"),
            &n,
        ))
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "token": "apptoken",
                "user": "userkey",
                "device": "droid2",
                "title": "api-prod",
                "message": "api-prod — major incident OPEN\nhttps://app.uptimepage.dev/i/7",
                "priority": 1,
                "timestamp": 1781251200_i64,
                "url": "https://app.uptimepage.dev/i/7",
                "url_title": "Open incident"
            })
        );
    }

    #[test]
    fn priority_follows_urgency_and_resolve() {
        let msg = |n: &IncidentNotice| {
            serde_json::to_value(PushoverNotifier::message("t", "u", None, n)).unwrap()
        };
        let low = notice(NotificationReason::Opened, IncidentUrgency::Low);
        assert_eq!(msg(&low)["priority"], 0);
        let mut resolved = notice(NotificationReason::Resolved, IncidentUrgency::High);
        resolved.ended_at = Some(Utc.with_ymd_and_hms(2026, 6, 12, 15, 0, 0).unwrap());
        let rv = msg(&resolved);
        assert_eq!(rv["priority"], -1);
        // The resolve push carries the resolve moment, not the open time.
        assert_eq!(rv["timestamp"], resolved.ended_at.unwrap().timestamp());
        let mut no_url = notice(NotificationReason::Opened, IncidentUrgency::High);
        no_url.url = None;
        let v = msg(&no_url);
        assert!(v.get("url").is_none());
        assert!(v.get("url_title").is_none());
    }
}
