use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverallState {
    Operational,
    Maintenance,
    MinorDisruption,
    PartialOutage,
    MajorOutage,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OverallStatus {
    pub state: OverallState,
    #[schema(example = "All Systems Operational")]
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicComponentStatus {
    Operational,
    Degraded,
    PartialOutage,
    MajorOutage,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DayState {
    Operational,
    Degraded,
    PartialOutage,
    MajorOutage,
    Maintenance,
    NoData,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicComponent {
    pub id: Uuid,
    pub name: String,
    #[schema(nullable = true)]
    pub description: Option<String>,
    pub current_status: PublicComponentStatus,
    /// Daily history, oldest first.
    pub history: Vec<DayState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicComponentGroup {
    #[schema(nullable = true, example = "API")]
    pub name: Option<String>,
    pub components: Vec<PublicComponent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum IncidentSeverity {
    Minor,
    #[default]
    Major,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatusPhase {
    Investigating,
    Identified,
    Monitoring,
    Resolved,
    Postmortem,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIncidentUpdate {
    pub posted_at: DateTime<Utc>,
    pub phase: IncidentStatusPhase,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIncident {
    pub id: Uuid,
    pub component_id: Uuid,
    pub component_name: String,
    /// `public_title` if set; otherwise auto-generated like `"API major outage"`.
    pub title: String,
    pub started_at: DateTime<Utc>,
    #[schema(nullable = true)]
    pub ended_at: Option<DateTime<Utc>>,
    pub severity: IncidentSeverity,
    /// Most recent phase from operator updates; `investigating` if none.
    pub status_phase: IncidentStatusPhase,
    pub updates: Vec<PublicIncidentUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicMaintenance {
    pub id: Uuid,
    pub title: String,
    #[schema(nullable = true)]
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub affected_component_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicMaintenanceList {
    pub active: Vec<PublicMaintenance>,
    pub upcoming: Vec<PublicMaintenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicStatusPage {
    pub overall: OverallStatus,
    pub generated_at: DateTime<Utc>,
    pub site_name: String,
    pub groups: Vec<PublicComponentGroup>,
    pub active_incidents: Vec<PublicIncident>,
    pub recent_incidents: Vec<PublicIncident>,
    pub active_maintenance: Vec<PublicMaintenance>,
    pub upcoming_maintenance: Vec<PublicMaintenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComponentHistoryResponse {
    pub component_id: Uuid,
    pub component_name: String,
    pub days: u32,
    pub history: Vec<DayState>,
}
