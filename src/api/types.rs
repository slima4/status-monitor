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
pub struct HeaderPreview {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TestResponse {
    pub result: CheckResult,
    /// Whether the check would be considered `up` given the spec's
    /// `expected_status` / `body_contains`.
    pub matched_expectations: bool,
    /// Validation warnings that did not block execution.
    pub warnings: Vec<String>,
    /// Response headers preview, HTTP only. Sensitive headers redacted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_headers_preview: Vec<HeaderPreview>,
    /// First 1 KiB of decoded body, HTTP only. UTF-8 lossy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_snippet: Option<String>,
}

/// Per-monitor rollup for the operator dashboard table. One row per target
/// over the chosen range — drives every numeric cell in the Dashboard list
/// (p50/p95/error rate/uptime%/last status). Produced by a single batched
/// ClickHouse aggregation so a 1k-monitor org renders in one round-trip.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardMetrics {
    pub target_id: Uuid,
    /// Number of check samples observed in the range.
    pub samples: u64,
    /// Samples with `status = up`.
    pub up: u64,
    /// Mean check duration in milliseconds. `0` when `samples == 0`.
    pub avg_ms: u32,
    /// Median check duration in milliseconds. `0` when `samples == 0`.
    pub p50_ms: u32,
    /// 95th-percentile check duration in milliseconds. `0` when `samples == 0`.
    pub p95_ms: u32,
    /// Latest observed status string ("up" / "down" / "degraded" / "error"),
    /// or empty when the range contains no samples.
    pub last_status: String,
}

/// One sparkline bucket — minute-aligned average duration. The dashboard
/// renders a fixed 60-minute trace per target so operators see a
/// "right-now" trend independent of the selected aggregation range.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DashboardSparkBucket {
    pub target_id: Uuid,
    /// Unix-seconds for the bucket's `toStartOfMinute(timestamp)`.
    pub bucket_ts: i64,
    pub avg_ms: f32,
}

/// Aggregate health for the period immediately before the selected range
/// — drives the Δ-vs-prior hints on each KPI card. Same shape as the
/// "current" totals so the view layer subtracts cleanly. `avg_ms = 0`
/// when there were no samples.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct PriorPeriodSummary {
    pub checks_total: u64,
    pub checks_up: u64,
    pub avg_ms: u32,
}

/// One slice of the fleet 24h uptime ribbon. Aggregates every monitor in
/// the org into a single bucket so the dashboard renders 48 × 30-minute
/// cells from a single matview merge — cost stays O(buckets), not O(orgs
/// × monitors).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FleetRibbonBucket {
    /// Unix-seconds at the bucket's start (`toStartOfInterval(minute, …)`).
    pub bucket_ts: i64,
    /// Total samples across every monitor in the bucket window.
    pub samples: u64,
    /// Samples with `status = up`.
    pub up: u64,
}
