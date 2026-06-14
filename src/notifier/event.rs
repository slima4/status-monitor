use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::domain::{IncidentSeverity, IncidentUrgency, NotificationReason};

/// The incident-shaped payload handed to a transport. Serialized as-is for the
/// generic webhook channel.
#[derive(Debug, Clone, Serialize)]
pub struct IncidentNotice {
    pub incident_id: Uuid,
    pub reason: NotificationReason,
    /// Monitor name; `None` for a manual incident not tied to a monitor.
    pub monitor_name: Option<String>,
    pub title: Option<String>,
    pub severity: IncidentSeverity,
    pub urgency: IncidentUrgency,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub error_sample: Option<String>,
    /// Regions down / still up at open time. Empty for a single-region monitor.
    pub regions_down: Vec<String>,
    pub regions_up: Vec<String>,
    /// Deep link to the incident detail page, when a base URL is configured.
    pub url: Option<String>,
}

impl IncidentNotice {
    /// Human label: monitor name, else title, else a generic fallback.
    pub fn label(&self) -> &str {
        self.monitor_name
            .as_deref()
            .or(self.title.as_deref())
            .unwrap_or("incident")
    }

    /// Whole-minute duration for a resolved notice, when `ended_at` is set.
    pub fn duration_minutes(&self) -> Option<i64> {
        self.ended_at
            .map(|end| (end - self.started_at).num_minutes().max(0))
    }

    /// True only when more than one region is in play; single-region monitors
    /// skip the breakdown line.
    pub fn has_region_breakdown(&self) -> bool {
        self.regions_down.len() + self.regions_up.len() > 1
    }

    /// `down: a, b{sep}up: c`, each region name passed through `esc`. `None` for
    /// a single-region monitor. Transports supply their own escaping + separator.
    pub fn region_summary(&self, esc: impl Fn(&str) -> String, sep: &str) -> Option<String> {
        if !self.has_region_breakdown() {
            return None;
        }
        let join = |rs: &[String]| rs.iter().map(|r| esc(r)).collect::<Vec<_>>().join(", ");
        let mut parts = Vec::new();
        if !self.regions_down.is_empty() {
            parts.push(format!("down: {}", join(&self.regions_down)));
        }
        if !self.regions_up.is_empty() {
            parts.push(format!("up: {}", join(&self.regions_up)));
        }
        Some(parts.join(sep))
    }

    /// Plain-text one-liner for chat-style transports (Telegram, WhatsApp):
    /// no markup, optional error sample, region breakdown and link on their
    /// own lines.
    pub fn plain_text(&self) -> String {
        let link = self
            .url
            .as_deref()
            .map(|u| format!("\n{u}"))
            .unwrap_or_default();
        let regions = self
            .region_summary(|r| r.to_string(), " · ")
            .map(|s| format!("\n{s}"))
            .unwrap_or_default();
        match self.reason {
            NotificationReason::Opened | NotificationReason::Escalated => format!(
                "{label} — {sev} incident OPEN{err}{regions}{link}",
                label = self.label(),
                sev = self.severity.as_db_str(),
                err = self
                    .error_sample
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default(),
            ),
            NotificationReason::Reopened => {
                format!("{label} — incident REOPENED{link}", label = self.label())
            }
            NotificationReason::Resolved => {
                let dur = self
                    .duration_minutes()
                    .map(|m| format!(" after {m}m"))
                    .unwrap_or_default();
                format!(
                    "{label} — incident RESOLVED{dur}{link}",
                    label = self.label()
                )
            }
            NotificationReason::NoData => format!(
                "{label} — NO DATA: monitoring interrupted, no check results received{link}",
                label = self.label()
            ),
            NotificationReason::DataResumed => format!(
                "{label} — monitoring RESUMED, receiving check results again{link}",
                label = self.label()
            ),
        }
    }
}
