use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::domain::{
    CheckResult, CheckStatus, IncidentSeverity, IncidentUrgency, NotificationReason, OrgId, Target,
};

/// Result-plus-target envelope produced by the worker pool and consumed by the
/// alert engine. Lives here (next to AlertEvent) so the worker's fan-out
/// dependency points into the notifier module, matching the producer→consumer
/// data flow.
///
/// `org_id` is the owning tenant of `target`, threaded from the scheduler's
/// cross-org enumeration so channel resolution stays org-scoped (a tenant's
/// target can only ever resolve that tenant's channels).
pub struct AlertSignal {
    pub target: Arc<Target>,
    pub org_id: OrgId,
    pub result: CheckResult,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Down,
    Recovered,
}

impl AlertKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Recovered => "recovered",
        }
    }
}

/// The payload handed to a notifier transport (and serialized as-is for the
/// generic webhook channel). Channel identity is no longer carried here — the
/// engine resolves the bound channel and picks the transport before building
/// this event.
#[derive(Debug, Clone, Serialize)]
pub struct AlertEvent {
    pub target_id: Uuid,
    pub target_name: String,
    pub kind: AlertKind,
    pub consecutive_failures: u32,
    pub last_status: CheckStatus,
    pub last_error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// The incident-shaped payload handed to a transport when paging is driven by
/// the incident lifecycle rather than the raw per-result threshold. Serialized
/// as-is for the generic webhook channel.
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
}
