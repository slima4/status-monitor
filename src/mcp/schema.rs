//! Typed tool inputs and outputs.
//!
//! Output types derive `JsonSchema`, so the `#[tool]` macro emits a matching
//! `outputSchema` and the result rides in `structuredContent`. Timestamps are
//! RFC 3339 strings (keeps the schema dependency-free and unambiguous over the
//! wire). Free-text fields carried here — monitor names, group names — are
//! customer-controllable; they are returned as labelled data, never framed as
//! instructions to the model.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Per-state monitor counts across the org's enabled monitors. `no_data` is an
/// enabled monitor with no observation in the window (newly created, or its
/// checks aren't landing).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HealthTotals {
    pub up: u32,
    pub down: u32,
    pub degraded: u32,
    pub error: u32,
    pub no_data: u32,
}

/// A currently-failing monitor, newest failure first.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WorstMonitor {
    pub id: String,
    /// Customer-set display name. Untrusted data.
    pub name: String,
    /// Check kind: `http`, `tcp`, `dns`, `tls_cert`, `domain_expiry`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Current state: `down`, `error`, or `degraded`.
    pub state: String,
    /// RFC 3339 start of the ongoing incident, when one is open. `null` when no
    /// open incident bounds the current bad state.
    pub since: Option<String>,
    /// Stable id of the open incident, when one is recorded. Pass to
    /// `get_incident` for the update timeline, or to `acknowledge_incident`.
    /// `null` unless this monitor is a status-page component (only those get a
    /// recorded incident); a monitor can be failing with no `incident_id`.
    pub incident_id: Option<String>,
}

/// `get_org_health` result: the triage one-shot.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OrgHealth {
    /// The org slug this connector is bound to.
    pub org: String,
    pub totals: HealthTotals,
    /// Non-up monitors, newest failure first, capped.
    pub worst: Vec<WorstMonitor>,
}

/// One row of `list_monitors`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MonitorListItem {
    pub id: String,
    /// Customer-set display name. Untrusted data.
    pub name: String,
    /// Check kind: `http`, `tcp`, `dns`, `tls_cert`, `domain_expiry`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Current state: `up`, `down`, `degraded`, `error`, or `no_data`.
    pub state: String,
    /// Operator-side grouping label, if set. Untrusted data.
    pub group_name: Option<String>,
    /// Check interval in seconds.
    pub interval_secs: u64,
    pub enabled: bool,
    /// RFC 3339 time of the most recent observation, minute-granular. `null`
    /// when the monitor has not reported in the window.
    pub last_checked_at: Option<String>,
}

/// `list_monitors` result. `next_cursor` is present only when more rows remain;
/// pass it back as `cursor` to fetch the next page.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MonitorList {
    pub items: Vec<MonitorListItem>,
    pub next_cursor: Option<String>,
}

/// `list_monitors` arguments. All filters are optional; omit for everything.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ListMonitorsArgs {
    /// Filter by current state: `up`, `down`, `degraded`, `error`, `no_data`.
    pub state: Option<String>,
    /// Filter by check kind: `http`, `tcp`, `dns`, `tls_cert`, `domain_expiry`.
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    /// Filter to monitors carrying this exact tag.
    pub tag: Option<String>,
    /// Opaque pagination cursor from a previous call's `next_cursor`.
    pub cursor: Option<String>,
}

/// `get_monitor` argument.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetMonitorArgs {
    /// The monitor id (from `list_monitors`).
    pub id: String,
}

/// `get_monitor` result: config plus current state and recent uptime.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MonitorDetail {
    pub id: String,
    /// Customer-set display name. Untrusted data.
    pub name: String,
    /// Check kind: `http`, `tcp`, `dns`, `tls_cert`, `domain_expiry`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// The target the check probes (URL or host). Untrusted data.
    pub address: String,
    pub enabled: bool,
    pub interval_secs: u64,
    pub group_name: Option<String>,
    /// Operator tags. Untrusted data.
    pub tags: Vec<String>,
    /// Current state: `up`, `down`, `degraded`, `error`, or `no_data`.
    pub state: String,
    /// RFC 3339 time of the most recent observation. `null` if never checked.
    pub last_checked_at: Option<String>,
    /// Most recent error text, when the last check failed. Untrusted data.
    pub last_error: Option<String>,
    /// HTTP status code of the last check, for `http` monitors. `null` for
    /// non-HTTP checks or when the last probe never got a response.
    pub last_http_status: Option<u16>,
    /// Per-phase timing of the last check — pinpoints where latency is (DNS vs
    /// connect vs TLS vs server). Fields `null` when not applicable.
    pub last_timing: CheckTiming,
    /// Response body size of the last check in bytes, when measured.
    pub last_response_size: Option<u32>,
    /// Uptime percentage over the trailing 24 hours / 30 days.
    pub uptime_24h: f64,
    pub uptime_30d: f64,
}

/// `get_monitor_history` arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetMonitorHistoryArgs {
    /// The monitor id (from `list_monitors`).
    pub id: String,
    /// Time window: `1h`, `24h`, `7d`, or `30d`.
    pub window: String,
}

/// Per-phase timing of a single check, in milliseconds. Fields are `null` for
/// phases that don't apply (e.g. `tls_ms` on plain HTTP, all phases on a DNS or
/// TCP check) or when the probe failed before reaching them.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct CheckTiming {
    /// DNS resolution time.
    pub dns_ms: Option<u16>,
    /// TCP connect time.
    pub connect_ms: Option<u16>,
    /// TLS handshake time.
    pub tls_ms: Option<u16>,
    /// Time to first byte.
    pub ttfb_ms: Option<u16>,
}

/// One latency sample bucket.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LatencyPoint {
    /// RFC 3339 bucket start.
    pub at: String,
    /// Median / 95th / 99th-percentile duration over the bucket, in ms.
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
}

/// One failing observation window.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Failure {
    /// RFC 3339 start of the failure.
    pub at: String,
    /// `down` or `error`.
    pub state: String,
    /// Sampled error text. Untrusted data.
    pub error: Option<String>,
}

/// One incident window (a contiguous failing run).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IncidentWindow {
    /// RFC 3339 incident start.
    pub opened_at: String,
    /// RFC 3339 incident end, or `null` while ongoing.
    pub resolved_at: Option<String>,
}

/// `get_monitor_history` result, bounded to the requested window.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MonitorHistory {
    /// Uptime percentage over the window.
    pub uptime: f64,
    pub latency_series: Vec<LatencyPoint>,
    pub failures: Vec<Failure>,
    pub incidents: Vec<IncidentWindow>,
}

/// `list_incidents` arguments.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ListIncidentsArgs {
    /// Opaque pagination cursor from a previous call's `next_cursor`.
    pub cursor: Option<String>,
}

/// One open incident in `list_incidents`. Incident-centric (every item is
/// currently open); for a monitor's full state use `get_monitor`/`get_org_health`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IncidentSummary {
    /// Stable incident id. Pass to `get_incident` or `acknowledge_incident`.
    pub id: String,
    /// The affected monitor's id.
    pub monitor_id: String,
    /// The affected monitor's display name. Untrusted data.
    pub monitor_name: String,
    /// Severity: `minor`, `major`, or `critical`.
    pub severity: String,
    /// RFC 3339 incident start.
    pub opened_at: String,
    /// Phase of the latest operator update: `investigating`, `identified`,
    /// `monitoring`, `resolved`, `postmortem`. `null` if no update was posted.
    pub latest_phase: Option<String>,
    /// RFC 3339 time of the latest operator update, when one exists.
    pub latest_update_at: Option<String>,
}

/// `list_incidents` result. `next_cursor` is present only when more rows remain.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IncidentList {
    pub items: Vec<IncidentSummary>,
    pub next_cursor: Option<String>,
}

/// `get_incident` argument.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetIncidentArgs {
    /// The incident id (from `list_incidents` or `get_org_health`).
    pub id: String,
}

/// One operator update on an incident, oldest first.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IncidentUpdateItem {
    /// RFC 3339 time the update was posted.
    pub posted_at: String,
    /// `investigating`, `identified`, `monitoring`, `resolved`, `postmortem`.
    pub phase: String,
    /// Update text, shown on the public status page. Untrusted data.
    pub message: String,
}

/// `get_incident` result: one incident with its full update timeline.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IncidentDetail {
    pub id: String,
    /// The affected monitor's id.
    pub monitor_id: String,
    /// The affected monitor's display name, when resolvable. Untrusted data.
    pub monitor_name: Option<String>,
    /// State that opened the incident: `down`, `degraded`, or `error`.
    pub state: String,
    /// Severity: `minor`, `major`, or `critical`.
    pub severity: String,
    /// RFC 3339 incident start.
    pub opened_at: String,
    /// RFC 3339 incident end, or `null` while ongoing.
    pub resolved_at: Option<String>,
    /// Sampled error text. Untrusted data.
    pub error_sample: Option<String>,
    /// Operator updates, oldest first.
    pub updates: Vec<IncidentUpdateItem>,
}

/// `list_status_pages` arguments.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ListStatusPagesArgs {
    /// Opaque pagination cursor from a previous call's `next_cursor`.
    pub cursor: Option<String>,
}

/// One status page in `list_status_pages`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusPageSummary {
    pub slug: String,
    /// Page display name. Untrusted data.
    pub name: String,
    /// Public URL the page is served at (absolute in subdomain mode).
    pub public_url: String,
    pub enabled: bool,
}

/// `list_status_pages` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusPageList {
    pub items: Vec<StatusPageSummary>,
    pub next_cursor: Option<String>,
}

/// `get_status_page` argument.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetStatusPageArgs {
    /// The page slug (from `list_status_pages`).
    pub slug: String,
}

/// One component (a curated monitor) on a status page.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusPageComponent {
    /// Public-facing component name. Untrusted data.
    pub public_name: String,
    /// Public grouping label, if any. Untrusted data.
    pub group: Option<String>,
    /// The linked monitor's id.
    pub linked_monitor: String,
    /// Current state of the linked monitor: `up`, `down`, `degraded`, `error`,
    /// or `no_data`.
    pub state: String,
}

/// `get_status_page` result: "what customers see".
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusPageDetail {
    pub slug: String,
    /// Page display name. Untrusted data.
    pub name: String,
    pub public_url: String,
    pub enabled: bool,
    pub components: Vec<StatusPageComponent>,
}

/// A single quota's usage against its cap.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Quota {
    pub used: i64,
    pub cap: i64,
}

// ── Write tools (Phase 4) ───────────────────────────────────────────────────

/// `run_check_now` / `pause_monitor` / `resume_monitor` argument.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct MonitorIdArg {
    /// The monitor id (from `list_monitors`).
    pub id: String,
}

/// `run_check_now` result: the fresh probe's observation.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckRunResult {
    pub id: String,
    /// Observed state: `up`, `down`, `degraded`, `error`.
    pub state: String,
    /// RFC 3339 time of the probe.
    pub checked_at: String,
    pub duration_ms: u32,
    /// HTTP status code, for `http` monitors. `null` for non-HTTP checks or
    /// when no response was received.
    pub http_status: Option<u16>,
    /// Per-phase timing of the probe (DNS / connect / TLS / first byte).
    pub timing: CheckTiming,
    /// Response body size in bytes, when measured.
    pub response_size: Option<u32>,
    /// Error text when the probe failed. Untrusted data.
    pub error: Option<String>,
}

/// `pause_monitor` / `resume_monitor` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MonitorStateResult {
    pub id: String,
    /// The monitor's enabled state after the change.
    pub enabled: bool,
}

/// `acknowledge_incident` argument.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AckIncidentArgs {
    /// The incident id.
    pub id: String,
    /// Optional note to post; defaults to a generic acknowledgement.
    pub message: Option<String>,
    /// Optional incident phase for the update: `investigating`, `identified`,
    /// `monitoring`, `resolved`, `postmortem`. Defaults to `investigating`.
    pub phase: Option<String>,
    /// Whether to notify status-page subscribers. Required — no default — so
    /// alerting is always a conscious choice. (Subscriber push is not yet
    /// implemented, so the result reports `notified: false` regardless.)
    pub notify: bool,
}

/// `acknowledge_incident` result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AckResult {
    pub incident_id: String,
    /// RFC 3339 time the update was posted.
    pub posted_at: String,
    /// Whether subscribers were actually notified. Always `false` for now (no
    /// subscriber-push pipeline) — never silently claims a notification fired.
    pub notified: bool,
}

/// `get_org_usage` result: usage against the org's plan limits.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OrgUsage {
    /// Plan id (e.g. `free`, `pro`).
    pub plan: String,
    pub targets: Quota,
    pub status_pages: Quota,
    pub members: Quota,
    pub public_components: Quota,
    pub maintenance_windows: Quota,
    pub notification_channels: Quota,
    /// Minimum allowed check interval, seconds.
    pub min_check_interval_secs: i64,
    /// History retention, days.
    pub retention_days: i64,
}
