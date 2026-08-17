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
    /// Explains a failed result. Never moves the verdict: assertions alone
    /// decide Up/Down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = true)]
    pub diagnostic: Option<CheckDiagnostic>,
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
            diagnostic: None,
            error: Some(reason.into()),
        }
    }

    pub fn is_served_stale(&self) -> bool {
        self.error
            .as_deref()
            .is_some_and(|e| e.starts_with(SERVED_STALE_PREFIX))
    }
}

/// Bounded diagnosis beside a check result. The HTTP error stays the
/// authority; this only attributes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckDiagnostic {
    pub kind: CheckDiagnosticKind,
    pub confidence: DiagnosticConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = true)]
    pub provider: Option<EdgeProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<DiagnosticEvidence>,
    /// Stable action codes for API clients. Derived from the kind, never
    /// stored, so an old row cannot outlive the advice it was written with.
    #[serde(
        default = "CheckDiagnostic::standard_access_remediations",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub remediations: Vec<DiagnosticRemediation>,
}

impl CheckDiagnostic {
    pub fn access_interference(
        confidence: DiagnosticConfidence,
        provider: Option<EdgeProvider>,
        evidence: Vec<DiagnosticEvidence>,
    ) -> Self {
        Self {
            kind: CheckDiagnosticKind::AccessInterference,
            confidence,
            provider,
            evidence,
            remediations: CheckDiagnosticKind::AccessInterference.remediations(),
        }
    }

    pub fn summary(&self) -> String {
        let certainty = match (self.kind, self.confidence) {
            (CheckDiagnosticKind::AccessInterference, DiagnosticConfidence::High) => {
                "access-policy block detected"
            }
            (CheckDiagnosticKind::AccessInterference, DiagnosticConfidence::Medium) => {
                "possible access-policy block"
            }
        };
        match self.provider {
            Some(provider) => format!("{certainty} at {}", provider.label()),
            None => certainty.to_string(),
        }
    }

    pub fn guidance(&self) -> &'static str {
        match self.kind {
            CheckDiagnosticKind::AccessInterference => {
                "use an authenticated health endpoint or exempt this monitor from browser challenges"
            }
        }
    }

    pub fn standard_access_remediations() -> Vec<DiagnosticRemediation> {
        CheckDiagnosticKind::AccessInterference.remediations()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckDiagnosticKind {
    AccessInterference,
}

impl CheckDiagnosticKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccessInterference => "access_interference",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "access_interference" => Some(Self::AccessInterference),
            _ => None,
        }
    }

    pub fn remediations(self) -> Vec<DiagnosticRemediation> {
        match self {
            Self::AccessInterference => vec![
                DiagnosticRemediation::UseAuthenticatedHealthEndpoint,
                DiagnosticRemediation::BypassBrowserChallengeForMonitor,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticConfidence {
    High,
    Medium,
}

impl DiagnosticConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeProvider {
    Akamai,
    AwsWaf,
    Cloudflare,
    AzureFrontDoor,
    DataDome,
    Vercel,
}

impl EdgeProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Akamai => "akamai",
            Self::AwsWaf => "aws_waf",
            Self::Cloudflare => "cloudflare",
            Self::AzureFrontDoor => "azure_front_door",
            Self::DataDome => "data_dome",
            Self::Vercel => "vercel",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Akamai => "the Akamai edge",
            Self::AwsWaf => "AWS WAF",
            Self::Cloudflare => "the Cloudflare edge",
            Self::AzureFrontDoor => "Azure Front Door",
            Self::DataDome => "DataDome",
            Self::Vercel => "the Vercel edge",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "akamai" => Some(Self::Akamai),
            "aws_waf" => Some(Self::AwsWaf),
            "cloudflare" => Some(Self::Cloudflare),
            "azure_front_door" => Some(Self::AzureFrontDoor),
            "data_dome" => Some(Self::DataDome),
            "vercel" => Some(Self::Vercel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEvidence {
    ChallengeHeader,
    EdgeServer,
    BlockPage,
    ReferenceId,
}

impl DiagnosticEvidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChallengeHeader => "challenge_header",
            Self::EdgeServer => "edge_server",
            Self::BlockPage => "block_page",
            Self::ReferenceId => "reference_id",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "challenge_header" => Some(Self::ChallengeHeader),
            "edge_server" => Some(Self::EdgeServer),
            "block_page" => Some(Self::BlockPage),
            "reference_id" => Some(Self::ReferenceId),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticRemediation {
    UseAuthenticatedHealthEndpoint,
    BypassBrowserChallengeForMonitor,
}

impl DiagnosticRemediation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UseAuthenticatedHealthEndpoint => "use_authenticated_health_endpoint",
            Self::BypassBrowserChallengeForMonitor => "bypass_browser_challenge_for_monitor",
        }
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
            diagnostic: None,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn old_diagnostic_payloads_receive_standard_remediations() {
        let diagnostic: CheckDiagnostic = serde_json::from_value(serde_json::json!({
            "kind": "access_interference",
            "confidence": "high",
            "provider": "akamai",
            "evidence": ["edge_server", "block_page"]
        }))
        .expect("old agent payload");

        assert_eq!(
            diagnostic.remediations,
            CheckDiagnostic::standard_access_remediations()
        );
    }

    #[test]
    fn edge_provider_storage_names_round_trip() {
        for provider in [
            EdgeProvider::Akamai,
            EdgeProvider::AwsWaf,
            EdgeProvider::Cloudflare,
            EdgeProvider::AzureFrontDoor,
            EdgeProvider::DataDome,
            EdgeProvider::Vercel,
        ] {
            assert_eq!(EdgeProvider::parse(provider.as_str()), Some(provider));
        }
        assert_eq!(EdgeProvider::parse("unknown_future_provider"), None);
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
