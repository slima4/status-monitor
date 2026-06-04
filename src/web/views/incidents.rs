//! Org-wide operator incidents console: a management surface for the
//! operational lifecycle (acknowledge / resolve / reopen / declare / note),
//! distinct from the per-monitor incident history under `/targets/{id}`.

use std::collections::HashMap;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::domain::{IncidentEvent, IncidentState, OpsIncident, OrgId, UserId};
use crate::error::AppError;
use crate::storage::orgs::list_members;
use crate::storage::{IncidentOpsFilter, TargetFilter};
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::{AuthedBrowser, CurrentOrg};

const STATE_FILTERS: &[&str] = &["all", "triggered", "acknowledged", "resolved"];
const PAGE_SIZES: &[usize] = &[25, 50, 100, 200];
const DEFAULT_PAGE_SIZE: usize = 50;

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub state: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub struct StateTab {
    pub key: &'static str,
    pub label: &'static str,
    pub active: bool,
}

pub struct PageSize {
    pub n: usize,
    pub active: bool,
}

pub struct ConsoleRow {
    pub id: String,
    pub target_id: Option<String>,
    pub label: String,
    pub state: &'static str,
    pub state_label: &'static str,
    pub severity: &'static str,
    pub urgency: &'static str,
    pub origin: &'static str,
    pub visibility: &'static str,
    pub acked_by: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub ongoing: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "incidents/list.html")]
pub struct IncidentsConsolePage {
    pub active_tab: &'static str,
    pub state: &'static str,
    pub state_tabs: Vec<StateTab>,
    pub rows: Vec<ConsoleRow>,
    // Pager (offset/limit; total drives "page N of M").
    pub limit: usize,
    pub total: usize,
    pub page: usize,
    pub total_pages: usize,
    pub range_lo: usize,
    pub range_hi: usize,
    pub prev_offset: Option<usize>,
    pub next_offset: Option<usize>,
    pub page_sizes: Vec<PageSize>,
}

fn state_label(s: IncidentState) -> &'static str {
    match s {
        IncidentState::Triggered => "Triggered",
        IncidentState::Acknowledged => "Acknowledged",
        IncidentState::Resolved => "Resolved",
    }
}

fn row_from(inc: OpsIncident, monitor_name: Option<String>, acked_by: Option<String>) -> ConsoleRow {
    let label = inc
        .title
        .clone()
        .or(monitor_name)
        .unwrap_or_else(|| "Untitled incident".to_string());
    ConsoleRow {
        id: inc.id.to_string(),
        target_id: inc.target_id.map(|t| t.to_string()),
        label,
        state: inc.state.as_db_str(),
        state_label: state_label(inc.state),
        severity: inc.severity.as_db_str(),
        urgency: inc.urgency.as_db_str(),
        origin: inc.origin.as_db_str(),
        visibility: inc.visibility.as_db_str(),
        acked_by,
        started_at: inc.started_at,
        ended_at: inc.ended_at,
        ongoing: inc.state.is_open(),
    }
}

fn parse_state(key: &str) -> Option<IncidentState> {
    match key {
        "triggered" => Some(IncidentState::Triggered),
        "acknowledged" => Some(IncidentState::Acknowledged),
        "resolved" => Some(IncidentState::Resolved),
        _ => None,
    }
}

/// Monitor id → name for the org, in one query (orgs are quota-capped, so a
/// single bounded fetch is cheaper than N per-incident lookups).
async fn name_map(state: &AppState, org: OrgId) -> WebResult<HashMap<Uuid, String>> {
    let targets = state
        .target_store
        .list(
            org,
            TargetFilter {
                limit: Some(10_000),
                ..Default::default()
            },
        )
        .await?;
    Ok(targets.into_iter().map(|t| (t.id, t.name)).collect())
}

/// User id → email label for the org, for rendering "acknowledged by …".
async fn members_map(state: &AppState, org: OrgId) -> WebResult<HashMap<UserId, String>> {
    let Some(pool) = &state.db else {
        return Ok(HashMap::new());
    };
    let members = list_members(pool, org).await?;
    Ok(members
        .into_iter()
        .map(|m| (m.membership.user_id, m.email))
        .collect())
}

pub async fn list(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> WebResult<IncidentsConsolePage> {
    let active: &'static str = STATE_FILTERS
        .iter()
        .copied()
        .find(|s| Some(*s) == params.state.as_deref())
        .unwrap_or("all");
    let st = parse_state(active);
    let limit = params
        .limit
        .filter(|n| PAGE_SIZES.contains(n))
        .unwrap_or(DEFAULT_PAGE_SIZE);

    let total = state.incident_ops_store.count(org, st).await?;
    let max_offset = if total == 0 { 0 } else { ((total - 1) / limit) * limit };
    // Snap a hand-typed offset down to a page boundary, then clamp to the last
    // populated page, so the page number and prev/next links stay aligned.
    let offset = ((params.offset.unwrap_or(0) / limit) * limit).min(max_offset);

    let incidents = state
        .incident_ops_store
        .list(
            org,
            IncidentOpsFilter {
                state: st,
                limit: Some(limit),
                offset,
            },
        )
        .await?;
    let names = name_map(&state, org).await?;
    let members = members_map(&state, org).await?;
    let rows: Vec<ConsoleRow> = incidents
        .into_iter()
        .map(|i| {
            let name = i.target_id.and_then(|t| names.get(&t).cloned());
            let acked = i.acknowledged_by.and_then(|u| members.get(&u).cloned());
            row_from(i, name, acked)
        })
        .collect();

    let shown = rows.len();
    let total_pages = if total == 0 { 1 } else { total.div_ceil(limit) };
    Ok(IncidentsConsolePage {
        active_tab: "incidents",
        state: active,
        state_tabs: STATE_FILTERS
            .iter()
            .map(|k| StateTab {
                key: k,
                label: k,
                active: *k == active,
            })
            .collect(),
        rows,
        limit,
        total,
        page: offset / limit + 1,
        total_pages,
        range_lo: if total == 0 { 0 } else { offset + 1 },
        range_hi: offset + shown,
        prev_offset: (offset > 0).then(|| offset.saturating_sub(limit)),
        next_offset: (offset + limit < total).then_some(offset + limit),
        page_sizes: PAGE_SIZES
            .iter()
            .map(|n| PageSize {
                n: *n,
                active: *n == limit,
            })
            .collect(),
    })
}

pub struct TimelineRow {
    pub kind: &'static str,
    pub actor: String,
    pub occurred_at: DateTime<Utc>,
    pub message: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "incidents/detail.html")]
pub struct IncidentDetailPage {
    pub active_tab: &'static str,
    pub id: String,
    pub label: String,
    pub target_id: Option<String>,
    pub monitor_name: Option<String>,
    pub state: &'static str,
    pub state_label: &'static str,
    pub severity: &'static str,
    pub urgency: &'static str,
    pub origin: &'static str,
    pub visibility: &'static str,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub error_sample: Option<String>,
    pub ongoing: bool,
    pub timeline: Vec<TimelineRow>,
}

fn event_kind_label(e: &IncidentEvent) -> &'static str {
    use crate::domain::IncidentEventKind::*;
    match e.kind {
        Triggered => "Triggered",
        Acknowledged => "Acknowledged",
        Assigned => "Assigned",
        Unassigned => "Unassigned",
        Escalated => "Escalated",
        Notified => "Notified",
        Note => "Note",
        SeverityChanged => "Severity changed",
        StateChanged => "State changed",
        Resolved => "Resolved",
        Reopened => "Reopened",
        Published => "Published",
        Unpublished => "Unpublished",
    }
}

pub async fn detail(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> WebResult<IncidentDetailPage> {
    let inc = state
        .incident_ops_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::INCIDENT_NOT_FOUND, "incident not found"))?;
    let monitor_name = match inc.target_id {
        Some(t) => state.target_store.get(org, t).await?.map(|x| x.name),
        None => None,
    };
    let acknowledged_by = match inc.acknowledged_by {
        Some(u) => members_map(&state, org).await?.get(&u).cloned(),
        None => None,
    };
    let events = state.incident_ops_store.timeline(org, id).await?;
    let timeline = events
        .iter()
        .map(|e| TimelineRow {
            kind: event_kind_label(e),
            actor: e.actor_type.as_db_str().to_string(),
            occurred_at: e.occurred_at,
            message: e.message.clone(),
        })
        .collect();
    let label = inc
        .title
        .clone()
        .or_else(|| monitor_name.clone())
        .unwrap_or_else(|| "Untitled incident".to_string());
    Ok(make_detail_page(inc, monitor_name, acknowledged_by, label, timeline))
}

fn make_detail_page(
    inc: OpsIncident,
    monitor_name: Option<String>,
    acknowledged_by: Option<String>,
    label: String,
    timeline: Vec<TimelineRow>,
) -> IncidentDetailPage {
    IncidentDetailPage {
        active_tab: "incidents",
        id: inc.id.to_string(),
        label,
        target_id: inc.target_id.map(|t| t.to_string()),
        monitor_name,
        state: inc.state.as_db_str(),
        state_label: state_label(inc.state),
        severity: inc.severity.as_db_str(),
        urgency: inc.urgency.as_db_str(),
        origin: inc.origin.as_db_str(),
        visibility: inc.visibility.as_db_str(),
        started_at: inc.started_at,
        ended_at: inc.ended_at,
        acknowledged_at: inc.acknowledged_at,
        acknowledged_by,
        error_sample: inc.error_sample.clone(),
        ongoing: inc.state.is_open(),
        timeline,
    }
}

pub struct MonitorOption {
    pub id: String,
    pub name: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "incidents/declare.html")]
pub struct DeclareIncidentPage {
    pub active_tab: &'static str,
    pub monitors: Vec<MonitorOption>,
}

pub async fn declare_form(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
) -> WebResult<DeclareIncidentPage> {
    let targets = state
        .target_store
        .list(
            org,
            TargetFilter {
                limit: Some(10_000),
                ..Default::default()
            },
        )
        .await?;
    let monitors = targets
        .into_iter()
        .map(|t| MonitorOption {
            id: t.id.to_string(),
            name: t.name,
        })
        .collect();
    Ok(DeclareIncidentPage {
        active_tab: "incidents",
        monitors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;

    fn ops(state: IncidentState) -> OpsIncident {
        OpsIncident {
            id: Uuid::now_v7(),
            target_id: Some(Uuid::now_v7()),
            title: None,
            state,
            severity: crate::domain::IncidentSeverity::Major,
            urgency: crate::domain::IncidentUrgency::High,
            origin: crate::domain::IncidentOrigin::Monitor,
            visibility: crate::domain::IncidentVisibility::Internal,
            started_at: Utc::now(),
            ended_at: None,
            acknowledged_at: None,
            acknowledged_by: None,
            assigned_to: None,
            resolved_by: None,
            escalation_policy_id: None,
            escalation_level: 0,
            escalation_round: 0,
            next_escalation_at: None,
            check_count: 2,
            error_sample: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn page(rows: Vec<ConsoleRow>) -> IncidentsConsolePage {
        IncidentsConsolePage {
            active_tab: "incidents",
            state: "all",
            state_tabs: STATE_FILTERS
                .iter()
                .map(|k| StateTab { key: k, label: k, active: *k == "all" })
                .collect(),
            rows,
            limit: 50,
            total: 0,
            page: 1,
            total_pages: 1,
            range_lo: 0,
            range_hi: 0,
            prev_offset: None,
            next_offset: None,
            page_sizes: PAGE_SIZES.iter().map(|n| PageSize { n: *n, active: *n == 50 }).collect(),
        }
    }

    #[test]
    fn console_empty_renders_empty_state() {
        let html = page(vec![]).render().unwrap();
        assert!(html.contains("No incidents match"));
    }

    #[test]
    fn console_triggered_row_shows_ack_and_resolve() {
        let row = row_from(ops(IncidentState::Triggered), Some("api-gateway".into()), None);
        let html = page(vec![row]).render().unwrap();
        assert!(html.contains("api-gateway"));
        assert!(html.contains(r#"data-incident-action="acknowledge""#));
        assert!(html.contains(r#"data-incident-action="resolve""#));
        assert!(!html.contains(r#"data-incident-action="reopen""#));
    }

    #[test]
    fn console_resolved_row_shows_reopen_only() {
        let mut inc = ops(IncidentState::Resolved);
        inc.ended_at = Some(Utc::now());
        let row = row_from(inc, Some("api".into()), None);
        let html = page(vec![row]).render().unwrap();
        assert!(html.contains(r#"data-incident-action="reopen""#));
        assert!(!html.contains(r#"data-incident-action="acknowledge""#));
    }

    #[test]
    fn console_shows_acked_by() {
        let mut inc = ops(IncidentState::Acknowledged);
        inc.acknowledged_at = Some(Utc::now());
        let row = row_from(inc, Some("api".into()), Some("alice@example.com".into()));
        let mut p = page(vec![row]);
        p.total = 1;
        p.range_lo = 1;
        p.range_hi = 1;
        let html = p.render().unwrap();
        assert!(html.contains("alice@example.com"));
    }

    #[test]
    fn detail_renders_actions_timeline_and_acker() {
        let mut inc = ops(IncidentState::Acknowledged);
        inc.title = Some("Payments degraded".into());
        inc.target_id = None;
        inc.origin = crate::domain::IncidentOrigin::Manual;
        inc.acknowledged_at = Some(Utc::now());
        let timeline = vec![TimelineRow {
            kind: "Triggered",
            actor: "user".to_string(),
            occurred_at: Utc::now(),
            message: None,
        }];
        let page = make_detail_page(
            inc,
            None,
            Some("alice@example.com".into()),
            "Payments degraded".to_string(),
            timeline,
        );
        let html = page.render().unwrap();
        assert!(html.contains("Payments degraded"));
        assert!(html.contains(r#"data-incident-note"#));
        assert!(html.contains("Activity"));
        assert!(html.contains("alice@example.com"));
    }

    #[test]
    fn detail_internal_shows_publish_public_shows_unpublish() {
        let internal = make_detail_page(ops(IncidentState::Triggered), None, None, "x".into(), vec![]);
        let html = internal.render().unwrap();
        assert!(html.contains(r#"data-incident-publish"#));
        assert!(!html.contains(r#"data-incident-unpublish"#));

        let mut pubinc = ops(IncidentState::Triggered);
        pubinc.visibility = crate::domain::IncidentVisibility::Public;
        let public = make_detail_page(pubinc, None, None, "x".into(), vec![]);
        let html = public.render().unwrap();
        assert!(html.contains(r#"data-incident-unpublish"#));
        assert!(!html.contains(r#"data-incident-publish"#));
    }
}
