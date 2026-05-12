use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub target_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub status: CheckStatus,
    pub duration_ms: u32,
    pub dns_ms: Option<u16>,
    pub connect_ms: Option<u16>,
    pub tls_ms: Option<u16>,
    pub ttfb_ms: Option<u16>,
    pub response_code: Option<u16>,
    pub response_size: Option<u32>,
    pub error: Option<String>,
}

impl CheckResult {
    pub fn error(target_id: Uuid, reason: impl Into<String>) -> Self {
        Self::error_with_elapsed(target_id, Utc::now(), 0, reason)
    }

    pub fn error_with_elapsed(
        target_id: Uuid,
        timestamp: DateTime<Utc>,
        duration_ms: u32,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            target_id,
            timestamp,
            status: CheckStatus::Error,
            duration_ms,
            dns_ms: None,
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Up,
    Down,
    Degraded,
    Error,
}
