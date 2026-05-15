//! Server-rendered public status page.
//!
//! Reads from the same `PublicSource` (and therefore the same in-process
//! cache) as the JSON endpoint, so a JSON and HTML request landing in the
//! same 10s window share one aggregator run.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::public_error::PublicAppError;
use crate::app::AppState;
use crate::config::PublicStatusConfig;
use crate::domain::{
    DayState, IncidentSeverity, IncidentStatusPhase, OrgId, OverallState, PublicComponent,
    PublicComponentGroup, PublicComponentStatus, PublicIncident, PublicIncidentUpdate,
    PublicMaintenance, PublicOrgBranding, PublicStatusPage,
};
use crate::public_status::{LocalDiskLogoStorage, LogoStorage};
use crate::storage::orgs::{OrgBranding, load_public_branding};
use crate::web::error::{NotFoundPage, UnavailablePage};
use crate::web::host::resolve_status_page_org;
use crate::web::views::{fmt_human, fmt_ts};

#[derive(Debug, Default, Deserialize)]
pub struct StatusParams {
    /// HTMX partial swap — return just the refresh region.
    pub fragment: Option<u8>,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/status.html")]
pub struct StatusFullPage {
    pub view: StatusView,
    pub branding: BrandingView,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/region.html")]
pub struct StatusRegion {
    pub view: StatusView,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/incident.html")]
pub struct IncidentDetailPage {
    pub branding: BrandingView,
    pub incident: IncidentDetailView,
    pub generated_iso: String,
    pub generated_human: String,
    pub rss_url: &'static str,
}

pub async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<StatusParams>,
) -> Response {
    let org = match resolve_status_page_org(&state, &headers).await {
        Ok(o) => o,
        Err(err) => return render_public_error(err),
    };
    let page = match state.public_source.page(org).await {
        Ok(p) => p,
        Err(err) => return render_public_error(err),
    };
    let view = build_view(&page);
    if params.fragment.unwrap_or(0) != 0 {
        // Chrome-free auto-refresh fragment: no header/footer/style, so the
        // branding lookup is skipped on the 30s poll.
        StatusRegion { view }.into_response()
    } else {
        let branding = resolve_branding(&state, org, &page.site_name).await;
        StatusFullPage { view, branding }.into_response()
    }
}

/// Serves the org's uploaded logo (or 404 when none is set). Same host→org
/// resolution as the page; the on-disk path is re-derived from the DB so the
/// query string is a cache-buster only, never a file selector.
pub async fn logo(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let org = match resolve_status_page_org(&state, &headers).await {
        Ok(o) => o,
        Err(err) => return render_public_error(err),
    };
    let Some(pool) = state.db.as_ref() else {
        return render_public_error(PublicAppError::NotFound);
    };
    let logo_path = match load_public_branding(pool, org).await {
        Ok(Some(ob)) => ob.branding.public_logo_path,
        Ok(None) => None,
        Err(e) => return render_public_error(PublicAppError::from(e)),
    };
    let Some(logo_path) = logo_path else {
        return render_public_error(PublicAppError::NotFound);
    };
    let store = LocalDiskLogoStorage::new(&state.cfg.public_status.logo_dir);
    match store.get(&logo_path).await {
        Ok(Some((bytes, content_type))) => (
            [
                (header::CONTENT_TYPE, content_type),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=3600, immutable".to_owned(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => render_public_error(PublicAppError::NotFound),
        Err(e) => render_public_error(PublicAppError::from(e)),
    }
}

pub async fn incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let org = match resolve_status_page_org(&state, &headers).await {
        Ok(o) => o,
        Err(err) => return render_public_error(err),
    };
    let (inc_res, page_res) = tokio::join!(
        state.public_source.incident_by_id(org, id),
        state.public_source.page(org),
    );
    let inc = match inc_res {
        Ok(i) => i,
        Err(err) => return render_public_error(err),
    };
    let fallback_name = match page_res {
        Ok(p) => p.site_name.clone(),
        Err(err) => return render_public_error(err),
    };
    let branding = resolve_branding(&state, org, &fallback_name).await;
    let now = Utc::now();
    IncidentDetailPage {
        branding,
        incident: IncidentDetailView::from_incident(&inc, now),
        generated_iso: fmt_ts(now),
        generated_human: fmt_human(now),
        rss_url: RSS_URL,
    }
    .into_response()
}

/// Maps a `PublicAppError` to an HTML response for the rendered routes —
/// avoids leaking the JSON envelope into the browser.
fn render_public_error(err: PublicAppError) -> Response {
    match err {
        PublicAppError::NotFound => {
            (StatusCode::NOT_FOUND, NotFoundPage { active_tab: "" }).into_response()
        }
        PublicAppError::InvalidDays | PublicAppError::BadRequest(_) => {
            (StatusCode::BAD_REQUEST, NotFoundPage { active_tab: "" }).into_response()
        }
        PublicAppError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            UnavailablePage { active_tab: "" },
        )
            .into_response(),
        PublicAppError::Internal(e) => {
            tracing::error!(error = %e, "public status page internal error");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                UnavailablePage { active_tab: "" },
            )
                .into_response()
        }
    }
}

// --- View model ----------------------------------------------------------

const RSS_URL: &str = "/api/public/v1/incidents.rss";
const HISTORY_LEN: usize = 90;
/// Single source of truth for the logo path — referenced by both the route
/// registration and the URL the template emits so they cannot drift.
pub const LOGO_ROUTE: &str = "/status/branding/logo";

pub struct StatusView {
    pub site_name: String,
    pub site_title: String,
    pub overall_label: String,
    pub overall_class: &'static str,
    pub overall_icon: &'static str,
    pub overall_aria: &'static str,
    pub generated_iso: String,
    pub generated_human: String,
    pub groups: Vec<GroupView>,
    pub active_incidents: Vec<IncidentSummary>,
    pub recent_incidents: Vec<IncidentSummary>,
    pub active_maintenance: Vec<MaintenanceView>,
    pub upcoming_maintenance: Vec<MaintenanceView>,
    pub has_active_incident: bool,
    pub has_maintenance: bool,
    pub has_components: bool,
    pub rss_url: &'static str,
    pub meta_description: String,
}

pub struct GroupView {
    pub heading: String,
    pub components: Vec<ComponentView>,
}

pub struct ComponentView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status_label: &'static str,
    pub status_class: &'static str,
    pub status_icon: &'static str,
    pub history: Vec<DayCell>,
    pub uptime_pct: String,
    pub history_summary: String,
}

pub struct DayCell {
    pub class: &'static str,
    pub label: &'static str,
    pub days_ago: usize,
}

/// Common header fields shared by the recent-incidents list and the detail page.
pub struct IncidentHeader {
    pub id: String,
    pub component_name: String,
    pub title: String,
    pub severity_label: &'static str,
    pub severity_class: &'static str,
    pub phase_label: &'static str,
    pub phase_class: &'static str,
    pub started_iso: String,
    pub started_human: String,
    pub ongoing: bool,
    pub duration_human: String,
}

pub struct IncidentSummary {
    pub header: IncidentHeader,
    pub ended_human: Option<String>,
    pub latest_message: Option<String>,
    pub permalink: String,
}

pub struct IncidentDetailView {
    pub header: IncidentHeader,
    pub ended: Option<TimePair>,
    pub updates: Vec<IncidentUpdateView>,
}

/// Pair of ISO timestamp + human label, kept together so templates always
/// have both halves (or neither) without per-field Option gymnastics.
pub struct TimePair {
    pub iso: String,
    pub human: String,
}

pub struct IncidentUpdateView {
    pub posted_iso: String,
    pub posted_human: String,
    pub phase_label: &'static str,
    pub phase_class: &'static str,
    pub message: String,
}

pub struct MaintenanceView {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub starts_iso: String,
    pub starts_human: String,
    pub ends_iso: String,
    pub ends_human: String,
    pub affects: String,
    pub starts_in_human: Option<String>,
}

/// Operator-controlled branding, resolved for rendering. Optional DB fields
/// have already had their defaults applied here, so the template just prints
/// these values — no fallback logic in the template layer.
pub struct BrandingView {
    pub display_name: String,
    /// Pre-sanitised HTML (markdown → ammonia allow-list). Rendered with the
    /// `safe` filter; it is the *only* unescaped value on the page.
    pub about_html: Option<String>,
    /// Already passed through [`safe_brand_color`]; safe to interpolate into
    /// the `:root` `<style>` block verbatim.
    pub brand_color: String,
    pub logo_url: Option<String>,
    pub show_powered_by: bool,
}

impl BrandingView {
    fn from_org(o: &OrgBranding, cfg: &PublicStatusConfig) -> Self {
        let display_name = o.resolved_display_name().to_owned();
        let about_html = o
            .branding
            .public_about
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(render_about);
        BrandingView {
            display_name,
            about_html,
            brand_color: safe_brand_color(
                o.branding.public_brand_color.as_deref(),
                &cfg.default_brand_color,
            ),
            logo_url: o
                .branding
                .public_logo_path
                .as_deref()
                .map(|p| format!("{LOGO_ROUTE}?v={p}")),
            show_powered_by: o.branding.show_powered_by(cfg.default_show_powered_by),
        }
    }
}

/// Loads the org's branding for a rendered page. A missing row, missing DB
/// handle, or query error degrades to defaults keyed off `fallback_name` —
/// the status page must still render if branding can't be read.
async fn resolve_branding(state: &AppState, org: OrgId, fallback_name: &str) -> BrandingView {
    let cfg = &state.cfg.public_status;
    if let Some(pool) = state.db.as_ref()
        && let Ok(Some(ob)) = load_public_branding(pool, org).await
    {
        return BrandingView::from_org(&ob, cfg);
    }
    BrandingView::from_org(
        &OrgBranding {
            name: fallback_name.to_owned(),
            slug: String::new(),
            branding: PublicOrgBranding::default(),
        },
        cfg,
    )
}

/// Independent, template-side re-validation of the brand colour. Trusts
/// neither the DB CHECK constraint nor the app-level validator: it owns its
/// own `^#[0-9a-fA-F]{6}$` predicate and returns `default` on any mismatch, so
/// the value interpolated into the `<style>` block can never break out of the
/// CSS rule even if an upstream layer's predicate is later widened.
pub fn safe_brand_color(raw: Option<&str>, default: &str) -> String {
    fn is_strict_hex(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
    }
    match raw {
        Some(c) if is_strict_hex(c) => c.to_owned(),
        _ => default.to_owned(),
    }
}

/// Allow-list sanitiser for `public_about`. Built once: the tag set and
/// builder are immutable, and `clean` takes `&self`, so there's no reason to
/// reconstruct it per render.
static ABOUT_SANITIZER: std::sync::LazyLock<ammonia::Builder<'static>> =
    std::sync::LazyLock::new(|| {
        let mut b = ammonia::Builder::default();
        b.tags(
            ["p", "strong", "em", "a", "br", "ul", "ol", "li"]
                .into_iter()
                .collect(),
        )
        .link_rel(Some("noopener nofollow"));
        b
    });

/// Renders `public_about` markdown to HTML, then strips everything outside a
/// small allow-list. The parser output is never trusted: ammonia is the
/// boundary, not pulldown-cmark.
pub fn render_about(markdown: &str) -> String {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    ABOUT_SANITIZER.clean(&html).to_string()
}

// --- Builders ------------------------------------------------------------

fn build_view(page: &PublicStatusPage) -> StatusView {
    let now = page.generated_at;

    let groups = page.groups.iter().map(build_group).collect::<Vec<_>>();
    let has_components = groups.iter().any(|g| !g.components.is_empty());

    let active = page
        .active_incidents
        .iter()
        .map(|i| build_incident_summary(i, now))
        .collect::<Vec<_>>();
    let recent = page
        .recent_incidents
        .iter()
        .map(|i| build_incident_summary(i, now))
        .collect::<Vec<_>>();

    let active_m = page
        .active_maintenance
        .iter()
        .map(|m| build_maintenance(m, now))
        .collect::<Vec<_>>();
    let upcoming_m = page
        .upcoming_maintenance
        .iter()
        .map(|m| build_maintenance(m, now))
        .collect::<Vec<_>>();

    let (overall_class, overall_icon, overall_aria) = overall_classes(page.overall.state);
    let site_title = format!("{} Status", page.site_name);
    let meta_description = format!("Live operational status for {}", page.site_name);

    StatusView {
        site_name: page.site_name.clone(),
        site_title,
        overall_label: page.overall.label.clone(),
        overall_class,
        overall_icon,
        overall_aria,
        generated_iso: fmt_ts(now),
        generated_human: fmt_human(now),
        groups,
        has_active_incident: !active.is_empty(),
        active_incidents: active,
        recent_incidents: recent,
        has_maintenance: !active_m.is_empty() || !upcoming_m.is_empty(),
        active_maintenance: active_m,
        upcoming_maintenance: upcoming_m,
        has_components,
        rss_url: RSS_URL,
        meta_description,
    }
}

fn build_group(g: &PublicComponentGroup) -> GroupView {
    GroupView {
        heading: g.name.clone().unwrap_or_else(|| "Other".to_string()),
        components: g.components.iter().map(build_component).collect(),
    }
}

fn build_component(c: &PublicComponent) -> ComponentView {
    let (status_label, status_class, status_icon) = component_classes(c.current_status);
    let history = build_history(&c.history);
    let (uptime_pct, summary) = history_stats(&c.history);
    ComponentView {
        id: c.id.to_string(),
        name: c.name.clone(),
        description: c.description.clone().filter(|s| !s.is_empty()),
        status_label,
        status_class,
        status_icon,
        history,
        uptime_pct,
        history_summary: summary,
    }
}

fn build_history(states: &[DayState]) -> Vec<DayCell> {
    let total = states.len().max(1);
    states
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let (class, label) = day_classes(*s);
            DayCell {
                class,
                label,
                days_ago: total - 1 - idx,
            }
        })
        .collect()
}

fn history_stats(states: &[DayState]) -> (String, String) {
    let mut with_data = 0usize;
    let mut bad = 0usize;
    let mut degraded = 0usize;
    for s in states {
        match s {
            DayState::NoData => {}
            DayState::MajorOutage | DayState::PartialOutage => {
                with_data += 1;
                bad += 1;
            }
            DayState::Degraded => {
                with_data += 1;
                degraded += 1;
            }
            DayState::Operational | DayState::Maintenance => with_data += 1,
        }
    }
    if with_data == 0 {
        return ("—".to_string(), format!("{HISTORY_LEN} days, no data"));
    }
    let pct = 100.0 - (bad as f64 / with_data as f64) * 100.0;
    let summary = if bad == 0 && degraded == 0 {
        format!("{with_data} days, no incidents")
    } else if bad == 0 {
        format!("{with_data} days, {degraded} degraded")
    } else {
        format!(
            "{with_data} days, {bad} outage{}, {degraded} degraded",
            if bad == 1 { "" } else { "s" },
        )
    };
    (format!("{pct:.2}"), summary)
}

fn build_incident_header(i: &PublicIncident, now: DateTime<Utc>) -> IncidentHeader {
    let ongoing = i.ended_at.is_none();
    let duration_end = i.ended_at.unwrap_or(now);
    let duration = (duration_end - i.started_at).max(ChronoDuration::zero());
    let (severity_label, severity_class) = severity_classes(i.severity);
    let (phase_label, phase_class) = phase_classes(i.status_phase);
    IncidentHeader {
        id: i.id.to_string(),
        component_name: i.component_name.clone(),
        title: i.title.clone(),
        severity_label,
        severity_class,
        phase_label,
        phase_class,
        started_iso: fmt_ts(i.started_at),
        started_human: fmt_human(i.started_at),
        ongoing,
        duration_human: humanize_duration(duration),
    }
}

fn build_incident_summary(i: &PublicIncident, now: DateTime<Utc>) -> IncidentSummary {
    let header = build_incident_header(i, now);
    let latest_message = i.updates.last().map(|u: &PublicIncidentUpdate| {
        if u.message.len() > 240 {
            format!("{}…", &u.message[..240])
        } else {
            u.message.clone()
        }
    });
    IncidentSummary {
        permalink: format!("/status/incidents/{}", header.id),
        header,
        ended_human: i.ended_at.map(fmt_human),
        latest_message,
    }
}

impl IncidentDetailView {
    fn from_incident(i: &PublicIncident, now: DateTime<Utc>) -> Self {
        let header = build_incident_header(i, now);
        let ended = i.ended_at.map(|t| TimePair {
            iso: fmt_ts(t),
            human: fmt_human(t),
        });
        let updates = i
            .updates
            .iter()
            .map(|u| {
                let (l, c) = phase_classes(u.phase);
                IncidentUpdateView {
                    posted_iso: fmt_ts(u.posted_at),
                    posted_human: fmt_human(u.posted_at),
                    phase_label: l,
                    phase_class: c,
                    message: u.message.clone(),
                }
            })
            .collect();
        Self {
            header,
            ended,
            updates,
        }
    }
}

fn build_maintenance(m: &PublicMaintenance, now: DateTime<Utc>) -> MaintenanceView {
    let starts_in = if m.starts_at > now {
        Some(humanize_duration(m.starts_at - now))
    } else {
        None
    };
    MaintenanceView {
        id: m.id.to_string(),
        title: m.title.clone(),
        description: m.description.clone().filter(|s| !s.is_empty()),
        starts_iso: fmt_ts(m.starts_at),
        starts_human: fmt_human(m.starts_at),
        ends_iso: fmt_ts(m.ends_at),
        ends_human: fmt_human(m.ends_at),
        affects: m.affected_component_names.join(", "),
        starts_in_human: starts_in,
    }
}

// --- Classifiers ---------------------------------------------------------

fn overall_classes(s: OverallState) -> (&'static str, &'static str, &'static str) {
    match s {
        OverallState::Operational => (
            "border-emerald-200 bg-emerald-50 text-emerald-800",
            "✓",
            "All systems operational",
        ),
        OverallState::Maintenance => (
            "border-sky-200 bg-sky-50 text-sky-800",
            "🛠",
            "Maintenance in progress",
        ),
        OverallState::MinorDisruption => (
            "border-amber-200 bg-amber-50 text-amber-900",
            "⚠",
            "Minor service disruption",
        ),
        OverallState::PartialOutage => (
            "border-orange-200 bg-orange-50 text-orange-900",
            "⚠",
            "Partial system outage",
        ),
        OverallState::MajorOutage => (
            "border-rose-200 bg-rose-50 text-rose-900",
            "✗",
            "Major system outage",
        ),
    }
}

fn component_classes(s: PublicComponentStatus) -> (&'static str, &'static str, &'static str) {
    match s {
        PublicComponentStatus::Operational => ("Operational", "text-emerald-700", "✓"),
        PublicComponentStatus::Degraded => ("Degraded", "text-amber-700", "⚠"),
        PublicComponentStatus::PartialOutage => ("Partial outage", "text-orange-700", "⚠"),
        PublicComponentStatus::MajorOutage => ("Major outage", "text-rose-700", "✗"),
        PublicComponentStatus::Maintenance => ("Maintenance", "text-sky-700", "🛠"),
    }
}

fn day_classes(s: DayState) -> (&'static str, &'static str) {
    match s {
        DayState::Operational => ("bg-emerald-500", "Operational"),
        DayState::Degraded => ("bg-amber-400", "Degraded"),
        DayState::PartialOutage => ("bg-orange-500", "Partial outage"),
        DayState::MajorOutage => ("bg-rose-600", "Major outage"),
        DayState::Maintenance => ("bg-sky-400", "Maintenance"),
        DayState::NoData => ("bg-slate-200", "No data"),
    }
}

fn severity_classes(s: IncidentSeverity) -> (&'static str, &'static str) {
    match s {
        IncidentSeverity::Minor => ("Minor", "bg-amber-100 text-amber-800"),
        IncidentSeverity::Major => ("Major", "bg-orange-100 text-orange-800"),
        IncidentSeverity::Critical => ("Critical", "bg-rose-100 text-rose-800"),
    }
}

fn phase_classes(p: IncidentStatusPhase) -> (&'static str, &'static str) {
    match p {
        IncidentStatusPhase::Investigating => ("Investigating", "bg-slate-100 text-slate-800"),
        IncidentStatusPhase::Identified => ("Identified", "bg-amber-100 text-amber-900"),
        IncidentStatusPhase::Monitoring => ("Monitoring", "bg-sky-100 text-sky-900"),
        IncidentStatusPhase::Resolved => ("Resolved", "bg-emerald-100 text-emerald-900"),
        IncidentStatusPhase::Postmortem => ("Postmortem", "bg-indigo-100 text-indigo-900"),
    }
}

// --- Formatting ----------------------------------------------------------

fn humanize_duration(d: ChronoDuration) -> String {
    let total = d.num_seconds().max(0);
    if total < 60 {
        return format!("{total}s");
    }
    let mins = total / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    let rem_mins = mins % 60;
    if hours < 24 {
        if rem_mins == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h {rem_mins}m");
    }
    let days = hours / 24;
    let rem_hours = hours % 24;
    if rem_hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d {rem_hours}h")
    }
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::OverallStatus;

    fn sample_page() -> PublicStatusPage {
        PublicStatusPage {
            overall: OverallStatus {
                state: OverallState::Operational,
                label: "All Systems Operational".into(),
            },
            generated_at: Utc::now(),
            site_name: "Acme".into(),
            groups: vec![PublicComponentGroup {
                name: Some("API".into()),
                components: vec![PublicComponent {
                    id: Uuid::nil(),
                    name: "Gateway".into(),
                    description: Some("Customer-facing edge".into()),
                    current_status: PublicComponentStatus::Operational,
                    history: vec![DayState::Operational; HISTORY_LEN],
                }],
            }],
            active_incidents: vec![],
            recent_incidents: vec![],
            active_maintenance: vec![],
            upcoming_maintenance: vec![],
        }
    }

    fn sample_branding() -> BrandingView {
        BrandingView::from_org(
            &OrgBranding {
                name: "Acme".into(),
                slug: "acme".into(),
                branding: PublicOrgBranding::default(),
            },
            &PublicStatusConfig::default(),
        )
    }

    #[test]
    fn full_page_renders_chrome_and_components() {
        let view = build_view(&sample_page());
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
        }
        .render()
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Acme Status"));
        assert!(html.contains("All Systems Operational"));
        assert!(html.contains("Gateway"));
        assert!(html.contains(r#"hx-get="/status?fragment=1""#));
        assert!(html.contains(r#"hx-trigger="every 30s""#));
        assert!(html.contains("data-tz"));
        assert!(html.contains("/static/js/htmx.min.js"));
        assert!(html.contains("/static/js/public/tz.js"));
        assert!(html.contains("/api/public/v1/incidents.rss"));
    }

    #[test]
    fn fragment_renders_region_without_doctype() {
        let view = build_view(&sample_page());
        let html = StatusRegion { view }.render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(!html.contains("<nav"));
        assert!(html.contains(r#"id="status-region""#));
        assert!(html.contains(r#"hx-trigger="every 30s""#));
        assert!(html.contains("Gateway"));
    }

    #[test]
    fn empty_page_renders_with_zero_components() {
        let mut p = sample_page();
        p.groups.clear();
        let view = build_view(&p);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
        }
        .render()
        .unwrap();
        assert!(html.contains("No public components"));
    }

    #[test]
    fn active_incident_banner_renders_when_present() {
        let mut p = sample_page();
        p.overall = OverallStatus {
            state: OverallState::MinorDisruption,
            label: "Minor Service Disruption".into(),
        };
        p.active_incidents.push(PublicIncident {
            id: Uuid::nil(),
            component_id: Uuid::nil(),
            component_name: "Gateway".into(),
            title: "Latency spike".into(),
            started_at: Utc::now() - ChronoDuration::minutes(14),
            ended_at: None,
            severity: IncidentSeverity::Major,
            status_phase: IncidentStatusPhase::Identified,
            updates: vec![PublicIncidentUpdate {
                posted_at: Utc::now() - ChronoDuration::minutes(2),
                phase: IncidentStatusPhase::Identified,
                message: "Rolling back the deploy.".into(),
            }],
        });
        let view = build_view(&p);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Active incident"));
        assert!(html.contains("Latency spike"));
        assert!(html.contains("Identified"));
        assert!(html.contains("Rolling back the deploy."));
    }

    #[test]
    fn maintenance_card_renders_when_present() {
        let mut p = sample_page();
        p.active_maintenance.push(PublicMaintenance {
            id: Uuid::nil(),
            title: "DB upgrade".into(),
            description: Some("Brief".into()),
            starts_at: Utc::now() - ChronoDuration::minutes(5),
            ends_at: Utc::now() + ChronoDuration::hours(1),
            affected_component_names: vec!["Gateway".into()],
        });
        let view = build_view(&p);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Scheduled maintenance"));
        assert!(html.contains("DB upgrade"));
        assert!(html.contains("Gateway"));
    }

    #[test]
    fn day_classes_cover_all_states() {
        for s in [
            DayState::Operational,
            DayState::Degraded,
            DayState::PartialOutage,
            DayState::MajorOutage,
            DayState::Maintenance,
            DayState::NoData,
        ] {
            let (class, label) = day_classes(s);
            assert!(!class.is_empty());
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn humanize_duration_picks_largest_unit() {
        assert_eq!(humanize_duration(ChronoDuration::seconds(45)), "45s");
        assert_eq!(humanize_duration(ChronoDuration::minutes(17)), "17m");
        assert_eq!(humanize_duration(ChronoDuration::minutes(134)), "2h 14m");
        assert_eq!(humanize_duration(ChronoDuration::hours(25)), "1d 1h");
        assert_eq!(humanize_duration(ChronoDuration::hours(48)), "2d");
    }

    #[test]
    fn history_stats_computes_uptime() {
        let mut h = vec![DayState::Operational; 90];
        h[10] = DayState::MajorOutage;
        let (pct, summary) = history_stats(&h);
        assert!(pct.starts_with("98"));
        assert!(summary.contains("1 outage"));
    }

    #[test]
    fn incident_detail_renders() {
        let inc = PublicIncident {
            id: Uuid::nil(),
            component_id: Uuid::nil(),
            component_name: "Gateway".into(),
            title: "Latency spike".into(),
            started_at: Utc::now() - ChronoDuration::minutes(30),
            ended_at: Some(Utc::now()),
            severity: IncidentSeverity::Major,
            status_phase: IncidentStatusPhase::Resolved,
            updates: vec![
                PublicIncidentUpdate {
                    posted_at: Utc::now() - ChronoDuration::minutes(25),
                    phase: IncidentStatusPhase::Investigating,
                    message: "Looking into it.".into(),
                },
                PublicIncidentUpdate {
                    posted_at: Utc::now() - ChronoDuration::minutes(5),
                    phase: IncidentStatusPhase::Resolved,
                    message: "Rolled back the deploy.".into(),
                },
            ],
        };
        let detail = IncidentDetailView::from_incident(&inc, Utc::now());
        let html = IncidentDetailPage {
            branding: sample_branding(),
            incident: detail,
            generated_iso: fmt_ts(Utc::now()),
            generated_human: fmt_human(Utc::now()),
            rss_url: RSS_URL,
        }
        .render()
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Latency spike"));
        assert!(html.contains("Investigating"));
        assert!(html.contains("Resolved"));
        assert!(html.contains("Rolled back the deploy."));
    }

    fn branding_with(b: PublicOrgBranding) -> BrandingView {
        BrandingView::from_org(
            &OrgBranding {
                name: "Acme".into(),
                slug: "acme".into(),
                branding: b,
            },
            &PublicStatusConfig::default(),
        )
    }

    #[test]
    fn safe_brand_color_accepts_strict_hex_only() {
        assert_eq!(safe_brand_color(Some("#a1B2c3"), "#000000"), "#a1B2c3");
        for bad in [
            "blue",
            "#abc",
            "#3b82f",
            "#3b82f60",
            "#zzzzzz",
            "  #3b82f6",
            "red; } body { display: none }",
        ] {
            assert_eq!(
                safe_brand_color(Some(bad), "#3b82f6"),
                "#3b82f6",
                "should reject {bad:?}"
            );
        }
        assert_eq!(safe_brand_color(None, "#3b82f6"), "#3b82f6");
    }

    #[test]
    fn render_about_strips_disallowed_and_keeps_allowlist() {
        let html = render_about("**bold** _em_ <script>alert(1)</script>\n\n- a\n- b");
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<em>em</em>"));
        assert!(html.contains("<li>a</li>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert(1)"));
    }

    #[test]
    fn render_about_adds_rel_to_links() {
        let html = render_about("[x](https://example.com)");
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("rel=\"noopener nofollow\""));
    }

    #[test]
    fn branding_renders_logo_about_and_color() {
        let view = build_view(&sample_page());
        let branding = branding_with(PublicOrgBranding {
            public_display_name: Some("Acme Public".into()),
            public_about: Some("**hi** there".into()),
            public_brand_color: Some("#ff0000".into()),
            public_logo_path: Some("acme-deadbeef.png".into()),
            public_show_powered_by: Some(false),
            ..PublicOrgBranding::default()
        });
        let html = StatusFullPage { view, branding }.render().unwrap();
        assert!(html.contains("Acme Public Status"));
        assert!(html.contains("--brand-color: #ff0000;"));
        assert!(html.contains(r#"src="/status/branding/logo?v=acme-deadbeef.png""#));
        assert!(html.contains("<strong>hi</strong> there"));
        assert!(!html.contains("Powered by status-monitor"));
    }

    #[test]
    fn powered_by_shown_by_default() {
        let view = build_view(&sample_page());
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Powered by status-monitor"));
    }

    // PRE-MORTEM PM #6: a relaxed DB/app validator must not let a crafted
    // brand colour break out of the `:root` rule. The template-side
    // sanitiser is the independent layer that holds even then.
    #[test]
    fn malicious_brand_color_cannot_escape_style_rule() {
        let view = build_view(&sample_page());
        let branding = branding_with(PublicOrgBranding {
            public_brand_color: Some("red; } body { display: none } /*".into()),
            ..PublicOrgBranding::default()
        });
        let html = StatusFullPage { view, branding }.render().unwrap();
        // Exactly one `--brand-color:` declaration, and it is the default —
        // `var(--brand-color)` uses have no colon so they don't match.
        assert_eq!(html.matches("--brand-color:").count(), 1);
        assert!(html.contains("--brand-color: #3b82f6;"));
        assert!(!html.contains("display: none"));
        assert!(!html.contains("} body {"));
    }

    #[test]
    fn branding_defaults_when_all_fields_null() {
        // With every override unset, the display name falls back to the org
        // name and no logo image is emitted (the header shows text). The
        // default colour and powered-by footer are covered by their own
        // tests above; this one pins the resolved-name + no-logo path.
        let view = build_view(&sample_page());
        let branding = branding_with(PublicOrgBranding::default());
        let html = StatusFullPage { view, branding }.render().unwrap();

        assert!(html.contains("Acme Status"), "display name = org name");
        assert!(
            !html.contains("/status/branding/logo"),
            "no logo img when path unset"
        );
    }
}
