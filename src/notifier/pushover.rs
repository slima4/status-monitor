use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::domain::{IncidentUrgency, NotificationReason};
use crate::error::{AppError, Result};
use crate::http_outbound::{OutboundHttpClient, get_json, post_json, post_json_capture};
use crate::notifier::event::IncidentNotice;
use crate::notifier::{Notifier, truncate_chars};

const MESSAGES_URL: &str = "https://api.pushover.net/1/messages.json";
// Pushover caps: message 1024, title 250, url 512 characters.
const MAX_MESSAGE_CHARS: usize = 1024;
const MAX_TITLE_CHARS: usize = 250;
const MAX_URL_CHARS: usize = 512;
// Emergency (priority 2): re-alert every RETRY seconds until acknowledged, for
// at most EXPIRE seconds. Pushover floors retry at 30 s and caps expire at 3 h.
const EMERGENCY_RETRY_SECS: u32 = 60;
const EMERGENCY_EXPIRE_SECS: u32 = 3600;

pub struct PushoverNotifier {
    client: OutboundHttpClient,
    token: String,
    user: String,
    device: Option<String>,
    emergency: bool,
    receipt: Mutex<Option<String>>,
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
    retry: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expire: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url_title: Option<&'static str>,
}

#[derive(Deserialize)]
struct SendResponse {
    receipt: Option<String>,
}

/// Emergency priority applies only to a live high-urgency page — not to a
/// resolve (which goes silent) or a low-urgency notice.
fn is_emergency(enabled: bool, notice: &IncidentNotice) -> bool {
    enabled
        && matches!(notice.urgency, IncidentUrgency::High)
        && !matches!(notice.reason, NotificationReason::Resolved)
}

impl PushoverNotifier {
    pub fn new(
        client: OutboundHttpClient,
        token: String,
        user: String,
        device: Option<String>,
        emergency: bool,
    ) -> Self {
        Self {
            client,
            token,
            user,
            device,
            emergency,
            receipt: Mutex::new(None),
        }
    }

    fn message<'a>(
        token: &'a str,
        user: &'a str,
        device: Option<&'a str>,
        emergency: bool,
        notice: &'a IncidentNotice,
    ) -> Message<'a> {
        let url = notice
            .url
            .as_deref()
            .map(|u| truncate_chars(u, MAX_URL_CHARS));
        let emergency = is_emergency(emergency, notice);
        Message {
            token,
            user,
            device,
            title: truncate_chars(notice.label(), MAX_TITLE_CHARS),
            message: truncate_chars(&notice.plain_text(), MAX_MESSAGE_CHARS),
            priority: if emergency {
                2
            } else {
                match (notice.reason, notice.urgency) {
                    (NotificationReason::Resolved, _) => -1,
                    (_, IncidentUrgency::High) => 1,
                    (_, IncidentUrgency::Low) => 0,
                }
            },
            retry: emergency.then_some(EMERGENCY_RETRY_SECS),
            expire: emergency.then_some(EMERGENCY_EXPIRE_SECS),
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
        let msg = Self::message(
            &self.token,
            &self.user,
            self.device.as_deref(),
            self.emergency,
            notice,
        );
        if msg.priority == 2 {
            let resp: SendResponse = post_json_capture(&self.client, &url, &msg).await?;
            if resp.receipt.is_none() {
                tracing::warn!("pushover accepted an emergency page but returned no receipt");
            }
            *self.receipt.lock().expect("receipt mutex") = resp.receipt;
            Ok(())
        } else {
            post_json(&self.client, &url, &msg).await
        }
    }

    fn taken_receipt(&self) -> Option<String> {
        self.receipt.lock().expect("receipt mutex").take()
    }
}

const RECEIPTS_BASE: &str = "https://api.pushover.net/1/receipts";

/// State of an emergency receipt at poll time.
pub struct ReceiptState {
    pub acknowledged: bool,
    pub expired: bool,
}

#[derive(Deserialize)]
struct ReceiptResponse {
    acknowledged: i32,
    expired: i32,
}

#[derive(Serialize)]
struct CancelBody<'a> {
    token: &'a str,
}

#[derive(Deserialize)]
struct CancelResponse {}

/// Poll/cancel the emergency-receipt lifecycle with the channel's application
/// token. Built per operation from the stored Pushover config.
pub struct PushoverReceipts {
    client: OutboundHttpClient,
    token: String,
}

impl PushoverReceipts {
    pub fn new(client: OutboundHttpClient, token: String) -> Self {
        Self { client, token }
    }

    fn url(&self, path: String) -> Result<Url> {
        path.parse()
            .map_err(|e| AppError::Other(anyhow::anyhow!("receipt url: {e}")))
    }

    /// Whether the recipient has acknowledged the page, and whether Pushover has
    /// stopped retrying it (acknowledged, expired, or cancelled).
    pub async fn poll(&self, receipt: &str) -> Result<ReceiptState> {
        // The token rides in the query string; scrub it from any error before it
        // reaches a log line.
        let url = self.url(format!(
            "{RECEIPTS_BASE}/{receipt}.json?token={}",
            self.token
        ))?;
        let r: ReceiptResponse = get_json(&self.client, &url).await.map_err(|e| {
            AppError::Other(anyhow::anyhow!(
                "pushover receipt poll: {}",
                e.to_string().replace(self.token.as_str(), "***")
            ))
        })?;
        Ok(ReceiptState {
            acknowledged: r.acknowledged == 1,
            expired: r.expired == 1,
        })
    }

    /// Stop the repeat loop for a receipt — the incident resolved before it was
    /// acknowledged.
    pub async fn cancel(&self, receipt: &str) -> Result<()> {
        let url = self.url(format!("{RECEIPTS_BASE}/{receipt}/cancel.json"))?;
        let _: CancelResponse =
            post_json_capture(&self.client, &url, &CancelBody { token: &self.token }).await?;
        Ok(())
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
            false,
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
            serde_json::to_value(PushoverNotifier::message("t", "u", None, false, n)).unwrap()
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

    #[test]
    fn emergency_only_arms_live_high_urgency_pages() {
        let msg = |n: &IncidentNotice| {
            serde_json::to_value(PushoverNotifier::message("t", "u", None, true, n)).unwrap()
        };
        // High-urgency open with emergency on: priority 2 + retry/expire.
        let v = msg(&notice(NotificationReason::Opened, IncidentUrgency::High));
        assert_eq!(v["priority"], 2);
        assert_eq!(v["retry"], EMERGENCY_RETRY_SECS);
        assert_eq!(v["expire"], EMERGENCY_EXPIRE_SECS);
        // Low urgency stays at 0 — emergency never escalates a non-critical page.
        let low = msg(&notice(NotificationReason::Opened, IncidentUrgency::Low));
        assert_eq!(low["priority"], 0);
        assert!(low.get("retry").is_none());
        // Resolves stay silent even with emergency on.
        let resolved = msg(&notice(NotificationReason::Resolved, IncidentUrgency::High));
        assert_eq!(resolved["priority"], -1);
        assert!(resolved.get("retry").is_none());
    }
}
