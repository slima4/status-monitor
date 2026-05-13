use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckSpec};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TagCount {
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct TargetsSummary {
    pub total: u64,
    pub enabled: u64,
    pub disabled: u64,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct StatusBreakdown {
    pub up: u64,
    pub down: u64,
    pub degraded: u64,
    pub error: u64,
    pub unknown: u64,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct Last24hSummary {
    pub checks_total: u64,
    pub checks_up: u64,
    #[schema(example = 99.94)]
    pub uptime_pct: f64,
    pub incidents: u64,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct SystemSummary {
    pub in_flight_checks: u32,
    pub result_queue_depth: u32,
    /// Cumulative drops since process start; reset on restart.
    pub dropped_results_last_5m: u64,
    pub circuit_breakers_open: u32,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct DashboardSummary {
    pub targets: TargetsSummary,
    pub current_status: StatusBreakdown,
    pub last_24h: Last24hSummary,
    pub system: SystemSummary,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BulkActionRequest {
    /// Up to 10 000 ids per request.
    pub ids: Vec<Uuid>,
    pub action: BulkAction,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BulkAction {
    Enable,
    Disable,
    Delete,
    TagAdd { tags: Vec<String> },
    TagRemove { tags: Vec<String> },
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkActionResponse {
    pub succeeded: Vec<Uuid>,
    pub failed: Vec<BulkActionFailure>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkActionFailure {
    pub id: Uuid,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TestRequest {
    pub check: CheckSpec,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TestResponse {
    pub result: CheckResult,
    /// Whether the check would be considered `up` given the spec's
    /// `expected_status` / `body_contains`.
    pub matched_expectations: bool,
    /// Validation warnings that did not block execution.
    pub warnings: Vec<String>,
}
