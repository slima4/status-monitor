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
    Ok(state.target_store.names(org).await?)
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
    /// Who acted: a member's email, `system` for automated transitions, or
    /// `former member` when the actor has since left the org.
    pub who: String,
    /// The actor drove this through the MCP server rather than the console.
    pub via_mcp: bool,
    pub occurred_at: DateTime<Utc>,
    pub message: Option<String>,
}

/// Resolve an event's actor to a human label + whether it came via MCP.
fn actor_label(e: &IncidentEvent, members: &HashMap<UserId, String>) -> (String, bool) {
    use crate::domain::ActorType;
    match e.actor_type {
        ActorType::System => ("system".to_string(), false),
        ActorType::User | ActorType::Mcp => {
            let who = match e.actor_id {
                Some(u) => members
                    .get(&u)
                    .cloned()
                    .unwrap_or_else(|| "former member".to_string()),
                None => "unknown".to_string(),
            };
            (who, e.actor_type == ActorType::Mcp)
        }
    }
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
    pub has_postmortem: bool,
    pub postmortem_published: bool,
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
        PostmortemPublished => "Postmortem published",
        PostmortemUnpublished => "Postmortem unpublished",
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
    let members = members_map(&state, org).await?;
    let acknowledged_by = inc.acknowledged_by.and_then(|u| members.get(&u).cloned());
    let events = state.incident_ops_store.timeline(org, id).await?;
    let timeline = events
        .iter()
        .map(|e| {
            let (who, via_mcp) = actor_label(e, &members);
            TimelineRow {
                kind: event_kind_label(e),
                who,
                via_mcp,
                occurred_at: e.occurred_at,
                message: e.message.clone(),
            }
        })
        .collect();
    let postmortem = state.postmortem_store.get(org, id).await?;
    let label = inc
        .title
        .clone()
        .or_else(|| monitor_name.clone())
        .unwrap_or_else(|| "Untitled incident".to_string());
    Ok(make_detail_page(
        inc,
        monitor_name,
        acknowledged_by,
        label,
        timeline,
        postmortem.as_ref(),
    ))
}

fn make_detail_page(
    inc: OpsIncident,
    monitor_name: Option<String>,
    acknowledged_by: Option<String>,
    label: String,
    timeline: Vec<TimelineRow>,
    postmortem: Option<&crate::domain::IncidentPostmortem>,
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
        has_postmortem: postmortem.is_some(),
        postmortem_published: postmortem.is_some_and(|p| p.published_at.is_some()),
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

// ── Reports ──────────────────────────────────────────────────────────────

const WINDOW_DAYS: &[u32] = &[7, 30, 90];
const DEFAULT_WINDOW: u32 = 30;

#[derive(Debug, Default, Deserialize)]
pub struct ReportParams {
    pub window_days: Option<u32>,
}

pub struct WindowOption {
    pub days: u32,
    pub active: bool,
}

pub struct ReportBucket {
    pub label: String,
    pub count: u64,
}

pub struct ReportMonitorRow {
    pub id: String,
    pub name: String,
    pub count: u64,
}

#[derive(Template, WebTemplate)]
#[template(path = "incidents/reports.html")]
pub struct IncidentsReportPage {
    pub active_tab: &'static str,
    pub window_days: u32,
    pub windows: Vec<WindowOption>,
    pub total: u64,
    pub mtta: Option<String>,
    pub mttr: Option<String>,
    pub by_severity: Vec<ReportBucket>,
    pub by_state: Vec<ReportBucket>,
    pub auto_resolved: u64,
    pub human_resolved: u64,
    pub top_monitors: Vec<ReportMonitorRow>,
}

/// Humanise a mean duration in seconds to a compact `1h 3m` / `5m 12s` / `8s`.
fn fmt_secs(secs: Option<f64>) -> Option<String> {
    let s = secs?.round().max(0.0) as u64;
    Some(if s >= 3600 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    })
}

pub async fn reports(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Query(params): Query<ReportParams>,
) -> WebResult<IncidentsReportPage> {
    let window = params
        .window_days
        .filter(|d| WINDOW_DAYS.contains(d))
        .unwrap_or(DEFAULT_WINDOW);
    let m = state.incident_ops_store.metrics(org, window).await?;
    let names = name_map(&state, org).await?;
    let bucket = |b: crate::domain::MetricBucket| ReportBucket {
        label: b.key,
        count: b.count,
    };
    Ok(IncidentsReportPage {
        active_tab: "incidents",
        window_days: m.window_days,
        windows: WINDOW_DAYS
            .iter()
            .map(|d| WindowOption {
                days: *d,
                active: *d == window,
            })
            .collect(),
        total: m.total,
        mtta: fmt_secs(m.mtta_secs),
        mttr: fmt_secs(m.mttr_secs),
        by_severity: m.by_severity.into_iter().map(bucket).collect(),
        by_state: m.by_state.into_iter().map(bucket).collect(),
        auto_resolved: m.auto_resolved,
        human_resolved: m.human_resolved,
        top_monitors: m
            .top_monitors
            .into_iter()
            .map(|t| ReportMonitorRow {
                id: t.target_id.to_string(),
                name: names
                    .get(&t.target_id)
                    .cloned()
                    .unwrap_or_else(|| "deleted monitor".to_string()),
                count: t.count,
            })
            .collect(),
    })
}

// ── Postmortem editor ─────────────────────────────────────────────────────

pub struct ActionItemModel {
    pub text: String,
    pub owner_user_id: String,
    pub done: bool,
}

pub struct MemberChoice {
    pub id: String,
    pub email: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "incidents/postmortem_form.html")]
pub struct PostmortemFormPage {
    pub active_tab: &'static str,
    pub incident_id: String,
    pub incident_label: String,
    pub exists: bool,
    pub published: bool,
    pub summary: String,
    pub root_cause: String,
    pub impact: String,
    pub action_items: Vec<ActionItemModel>,
    pub members: Vec<MemberChoice>,
}

pub async fn postmortem_form(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> WebResult<PostmortemFormPage> {
    let inc = state
        .incident_ops_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::INCIDENT_NOT_FOUND, "incident not found"))?;
    let monitor_name = match inc.target_id {
        Some(t) => state.target_store.get(org, t).await?.map(|x| x.name),
        None => None,
    };
    let incident_label = inc
        .title
        .clone()
        .or(monitor_name)
        .unwrap_or_else(|| "Untitled incident".to_string());

    let pm = state.postmortem_store.get(org, id).await?;
    let members: Vec<MemberChoice> = members_map(&state, org)
        .await?
        .into_iter()
        .map(|(uid, email)| MemberChoice {
            id: uid.to_string(),
            email,
        })
        .collect();

    let action_items = pm
        .as_ref()
        .map(|p| {
            p.action_items
                .iter()
                .map(|a| ActionItemModel {
                    text: a.text.clone(),
                    owner_user_id: a.owner_user_id.map(|u| u.to_string()).unwrap_or_default(),
                    done: a.done,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PostmortemFormPage {
        active_tab: "incidents",
        incident_id: id.to_string(),
        incident_label,
        exists: pm.is_some(),
        published: pm.as_ref().is_some_and(|p| p.published_at.is_some()),
        summary: pm.as_ref().and_then(|p| p.summary.clone()).unwrap_or_default(),
        root_cause: pm.as_ref().and_then(|p| p.root_cause.clone()).unwrap_or_default(),
        impact: pm.as_ref().and_then(|p| p.impact.clone()).unwrap_or_default(),
        action_items,
        members,
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
            who: "alice@example.com".to_string(),
            via_mcp: false,
            occurred_at: Utc::now(),
            message: None,
        }];
        let page = make_detail_page(
            inc,
            None,
            Some("alice@example.com".into()),
            "Payments degraded".to_string(),
            timeline,
            None,
        );
        let html = page.render().unwrap();
        assert!(html.contains("Payments degraded"));
        assert!(html.contains(r#"data-incident-note"#));
        assert!(html.contains("Activity"));
        assert!(html.contains("alice@example.com"));
    }

    #[test]
    fn detail_internal_shows_publish_public_shows_unpublish() {
        let internal =
            make_detail_page(ops(IncidentState::Triggered), None, None, "x".into(), vec![], None);
        let html = internal.render().unwrap();
        assert!(html.contains(r#"data-incident-publish"#));
        assert!(!html.contains(r#"data-incident-unpublish"#));

        let mut pubinc = ops(IncidentState::Triggered);
        pubinc.visibility = crate::domain::IncidentVisibility::Public;
        let public = make_detail_page(pubinc, None, None, "x".into(), vec![], None);
        let html = public.render().unwrap();
        assert!(html.contains(r#"data-incident-unpublish"#));
        assert!(!html.contains(r#"data-incident-publish"#));
    }

    #[test]
    fn actor_label_resolves_who_and_mcp() {
        use crate::domain::{ActorType, IncidentEventKind};
        let u = UserId(Uuid::now_v7());
        let mut members = HashMap::new();
        members.insert(u, "alice@example.com".to_string());
        let ev = |actor_type, actor_id| IncidentEvent {
            id: Uuid::now_v7(),
            incident_id: Uuid::now_v7(),
            occurred_at: Utc::now(),
            kind: IncidentEventKind::Acknowledged,
            actor_type,
            actor_id,
            detail: serde_json::Value::Null,
            message: None,
        };
        assert_eq!(actor_label(&ev(ActorType::System, None), &members), ("system".into(), false));
        assert_eq!(
            actor_label(&ev(ActorType::User, Some(u)), &members),
            ("alice@example.com".into(), false)
        );
        assert_eq!(
            actor_label(&ev(ActorType::Mcp, Some(u)), &members),
            ("alice@example.com".into(), true)
        );
        // An actor who has left the org no longer resolves to an email.
        let gone = UserId(Uuid::now_v7());
        assert_eq!(
            actor_label(&ev(ActorType::User, Some(gone)), &members),
            ("former member".into(), false)
        );
    }

    #[test]
    fn detail_shows_write_vs_edit_postmortem() {
        let none = make_detail_page(ops(IncidentState::Resolved), None, None, "x".into(), vec![], None);
        assert!(none.render().unwrap().contains("write postmortem"));

        let pm = crate::domain::IncidentPostmortem {
            incident_id: Uuid::now_v7(),
            summary: Some("s".into()),
            root_cause: None,
            impact: None,
            action_items: vec![],
            author_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            published_at: Some(Utc::now()),
        };
        let with = make_detail_page(ops(IncidentState::Resolved), None, None, "x".into(), vec![], Some(&pm));
        let html = with.render().unwrap();
        assert!(html.contains("edit postmortem"));
        assert!(html.contains("published"));
    }

    #[test]
    fn fmt_secs_humanises() {
        assert_eq!(fmt_secs(None), None);
        assert_eq!(fmt_secs(Some(8.0)).as_deref(), Some("8s"));
        assert_eq!(fmt_secs(Some(312.0)).as_deref(), Some("5m 12s"));
        assert_eq!(fmt_secs(Some(3780.0)).as_deref(), Some("1h 3m"));
    }

    #[test]
    fn reports_page_renders_kpis_and_top_monitors() {
        let page = IncidentsReportPage {
            active_tab: "incidents",
            window_days: 30,
            windows: WINDOW_DAYS
                .iter()
                .map(|d| WindowOption { days: *d, active: *d == 30 })
                .collect(),
            total: 4,
            mtta: Some("5m 0s".into()),
            mttr: Some("1h 2m".into()),
            by_severity: vec![ReportBucket { label: "major".into(), count: 3 }],
            by_state: vec![ReportBucket { label: "resolved".into(), count: 2 }],
            auto_resolved: 1,
            human_resolved: 1,
            top_monitors: vec![ReportMonitorRow {
                id: Uuid::now_v7().to_string(),
                name: "api-gateway".into(),
                count: 2,
            }],
        };
        let html = page.render().unwrap();
        assert!(html.contains("5m 0s"));
        assert!(html.contains("1h 2m"));
        assert!(html.contains("api-gateway"));
        assert!(html.contains("last 7d"));
    }

    #[test]
    fn postmortem_form_renders_fields_and_publish() {
        let page = PostmortemFormPage {
            active_tab: "incidents",
            incident_id: Uuid::now_v7().to_string(),
            incident_label: "Payments degraded".into(),
            exists: true,
            published: false,
            summary: "cache stampede".into(),
            root_cause: String::new(),
            impact: String::new(),
            action_items: vec![ActionItemModel {
                text: "add jitter".into(),
                owner_user_id: String::new(),
                done: false,
            }],
            members: vec![MemberChoice {
                id: Uuid::now_v7().to_string(),
                email: "alice@example.com".into(),
            }],
        };
        let html = page.render().unwrap();
        assert!(html.contains("cache stampede"));
        assert!(html.contains("add jitter"));
        assert!(html.contains("alice@example.com"));
        assert!(html.contains(r#"data-postmortem-publish="true""#));
    }
}
