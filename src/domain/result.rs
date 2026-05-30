use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Prefix on the `error` field of results synthesized by the domain-expiry
/// executor when serving a cached last-good answer instead of a fresh
/// probe. Internal annotation — every renderer must strip it before
/// showing the field to a customer.
pub const SERVED_STALE_PREFIX: &str = "served_stale:";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckResult {
    pub target_id: Uuid,
    /// Owning tenant of the target. Required (no default) so the type
    /// system forces every result to carry the real org — a wrong/missing
    /// org silently mis-files results under another tenant.
    pub org_id: Uuid,
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
    pub fn error(target_id: Uuid, org_id: Uuid, reason: impl Into<String>) -> Self {
        Self::error_with_elapsed(target_id, org_id, Utc::now(), 0, reason)
    }

    pub fn error_with_elapsed(
        target_id: Uuid,
        org_id: Uuid,
        timestamp: DateTime<Utc>,
        duration_ms: u32,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            target_id,
            org_id,
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

    pub fn is_served_stale(&self) -> bool {
        self.error
            .as_deref()
            .is_some_and(|e| e.starts_with(SERVED_STALE_PREFIX))
    }
}

/// The error string with the operator-only `served_stale:` annotation removed,
/// leaving the wrapped reason — the trailing JSON detail, or the bare reason
/// for a non-stale error. Returns `None` when a stale annotation wraps nothing
/// renderable. Every customer-facing surface must route `error` through this.
pub fn strip_served_stale(raw: &str) -> Option<&str> {
    match raw.strip_prefix(SERVED_STALE_PREFIX) {
        None => Some(raw),
        Some(rest) => rest.find('{').map(|i| &rest[i..]),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Up,
    Down,
    Degraded,
    Error,
}

impl CheckStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Degraded => "degraded",
            Self::Error => "error",
        }
    }

    pub const fn as_enum8(self) -> i8 {
        match self {
            Self::Up => 1,
            Self::Down => 2,
            Self::Degraded => 3,
            Self::Error => 4,
        }
    }

    pub const fn from_enum8(v: i8) -> Self {
        match v {
            1 => Self::Up,
            2 => Self::Down,
            3 => Self::Degraded,
            _ => Self::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(status: CheckStatus, error: Option<&str>) -> CheckResult {
        CheckResult {
            target_id: Uuid::nil(),
            org_id: Uuid::nil(),
            timestamp: Utc::now(),
            status,
            duration_ms: 0,
            dns_ms: None,
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn strip_served_stale_unwraps_or_passes_through() {
        // Non-stale errors pass through untouched.
        assert_eq!(strip_served_stale("no response"), Some("no response"));
        // Stale annotation with a wrapped JSON detail → surface the JSON.
        assert_eq!(
            strip_served_stale("served_stale: age=10; {\"domain\":\"x\"}"),
            Some("{\"domain\":\"x\"}")
        );
        // Stale annotation with nothing renderable → None.
        assert_eq!(strip_served_stale("served_stale: age=10"), None);
    }

    #[test]
    fn is_served_stale_matches_prefix() {
        assert!(
            synth(CheckStatus::Degraded, Some("served_stale: age=10; details")).is_served_stale()
        );
        assert!(synth(CheckStatus::Up, Some("served_stale: age=0")).is_served_stale());
        assert!(!synth(CheckStatus::Degraded, Some("upstream 503")).is_served_stale());
        assert!(!synth(CheckStatus::Degraded, None).is_served_stale());
    }
}
