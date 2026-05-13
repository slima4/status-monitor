use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::domain::{AlertChannel, CheckResult, CheckStatus, Target};

/// Result-plus-target envelope produced by the worker pool and consumed by the
/// alert engine. Lives here (next to AlertEvent) so the worker's fan-out
/// dependency points into the notifier module, matching the producer→consumer
/// data flow.
pub struct AlertSignal {
    pub target: Arc<Target>,
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

#[derive(Debug, Clone, Serialize)]
pub struct AlertEvent {
    pub target_id: Uuid,
    pub target_name: String,
    #[serde(serialize_with = "serialize_channel")]
    pub channel: AlertChannel,
    pub kind: AlertKind,
    pub consecutive_failures: u32,
    pub last_status: CheckStatus,
    pub last_error: Option<String>,
    pub timestamp: DateTime<Utc>,
    /// Per-channel recipient list (empty for non-email channels).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<String>,
}

fn serialize_channel<S: serde::Serializer>(c: &AlertChannel, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(c.as_str())
}
