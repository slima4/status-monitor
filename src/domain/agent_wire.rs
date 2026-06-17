//! Wire contract between the control plane and regional agents: config-pull,
//! result-ingest, and ad-hoc dispatch payloads. Serializable DTOs depending
//! only on other `domain` types so both sides share one definition.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckSpec, Target};

/// HTTP response-header name/value preview returned by a probe. Lives in the
/// wire contract (not `api::types`) so the contract owns every type it
/// serializes; `api::types` re-exports it for the public `TestResponse`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HeaderPreview {
    pub name: String,
    pub value: String,
}

/// One enabled target served to an agent on config pull.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTargetDto {
    pub org_id: Uuid,
    pub target: Target,
}

/// Config-pull response: every enabled target assigned to the agent's region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTargetsResponse {
    pub region: String,
    pub targets: Vec<AgentTargetDto>,
}

/// Result-ingest request body (owned; control-plane decode side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    pub batch_id: Uuid,
    pub results: Vec<CheckResult>,
}

/// Borrowed mirror of [`IngestRequest`] so the agent serializes a result batch
/// without cloning. Wire-identical to `IngestRequest`.
#[derive(Debug, Serialize)]
pub struct IngestRequestRef<'a> {
    pub batch_id: Uuid,
    pub results: &'a [CheckResult],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResponse {
    pub accepted: usize,
    /// Rows silently dropped (future-skewed clock or not assigned to this
    /// region) so a bad clock or stale assignment is visible rather than
    /// poisoning the whole batch.
    pub dropped: usize,
    pub duplicate: bool,
}

/// Which interactive surface an ad-hoc check serves. `test` results are
/// ephemeral (held only until the waiting request reads them); `check_now`
/// results are persisted like a scheduled check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchKind {
    Test,
    CheckNow,
}

/// An ad-hoc check handed to an agent on claim. Carries the decrypted check spec
/// so the agent can probe without DB access, exactly like config pull.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchedCheck {
    pub id: Uuid,
    pub kind: DispatchKind,
    pub org_id: Uuid,
    /// `None` for `test` (no stored target); the target's id for `check_now`.
    pub target_id: Option<Uuid>,
    pub spec: CheckSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchBatch {
    pub checks: Vec<DispatchedCheck>,
}

/// Result an agent posts back for one claimed check. `result` is always present
/// (a failed probe still yields a `CheckResult`); the probe preview is HTTP
/// `test` detail only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchReport {
    pub check_id: Uuid,
    pub result: CheckResult,
    #[serde(default)]
    pub response_headers_preview: Vec<HeaderPreview>,
    #[serde(default)]
    pub response_body_snippet: Option<String>,
}
