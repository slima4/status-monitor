use std::sync::Arc;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::domain::{CheckResult, Incident, OrgId};
use crate::error::AppError;
use crate::storage::{IncidentListQuery, TimeRange, UptimeStats};
use crate::web::error::{WebError, WebResult};
use crate::web::filters;
use crate::web::views::{
    RangeOption, build_range_options, describe_check, fmt_human, fmt_ts, resolve_range_key,
};
use crate::web::{AuthedBrowser, CurrentOrg};

// A raw row per check floods the page; the latency/breakdown charts above
// already carry the trend. 60 ≈ the last hour at a 1-minute interval —
// enough to eyeball recent behaviour, full history is the JSON API.
const RESULTS_PAGE_LIMIT: usize = 60;
const RANGE_KEYS: [&str; 4] = ["1h", "24h", "7d", "30d"];
const DEFAULT_RANGE: &str = "24h";
// Decoupled from the user's chart range so the header badge reflects the
// monitor's actual current state, not "no data" when the user picked 1h
// but the last check was 2h ago.
const LAST_RESULT_WINDOW_DAYS: i64 = 7;

const SUBTAB_MONITOR: &str = "monitor";
const SUBTAB_INCIDENTS: &str = "incidents";

const INCIDENT_RANGE_KEYS: [&str; 4] = ["24h", "7d", "30d", "90d"];
const INCIDENT_DEFAULT_RANGE: &str = "30d";
const INCIDENTS_PAGE_LIMIT: usize = 100;

fn ongoing_from_status(status: &str) -> usize {
    // Loose semantic: badge tracks "current state is bad", not a
    // separate CH scan. Misses the "fixed 30s ago, badge still set"
    // case in exchange for zero extra queries on the highest-traffic
    // page. Coalesce only ever opens one trailing run per target so
    // the count is 0 or 1.
    if matches!(status, "down" | "error") {
        1
    } else {
        0
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct DetailParams {
    #[serde(default)]
    pub range: Option<String>,
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct ResultRow {
    pub timestamp: String,
    pub status: &'static str,
    pub duration_ms: u32,
    pub response_code: String,
    pub error: String,
}

pub struct IncidentRow {
    pub id: Uuid,
    /// "down" | "error" | "degraded" — drives the `status-badge--*` CSS class.
    pub severity: &'static str,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// `Some(secs)` for closed incidents (template renders via
    /// `humanize_dur`); `None` while ongoing (template renders "Ongoing").
    pub duration_secs: Option<i64>,
    pub check_count: u64,
    pub error_sample: String,
    pub ongoing: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/partials/detail_live.html")]
pub struct DetailLive {
    pub id: String,
    pub name: String,
    pub range: &'static str,
    pub uptime: Arc<UptimeStatsView>,
    pub results: Arc<[ResultRow]>,
    pub results_has_more: bool,
    pub last_at_iso: Arc<str>,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/incidents.html")]
pub struct IncidentsPage {
    pub active_tab: &'static str,
    pub subtab: &'static str,
    pub ongoing_count: usize,
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub interval_s: u64,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub last_status: &'static str,
    pub last_at_iso: String,
    pub incidents: Vec<IncidentRow>,
    pub incidents_has_more: bool,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub range_base_path: String,
    pub from_iso: String,
    pub to_iso: String,
    pub from_human: String,
    pub to_human: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/detail.html")]
pub struct DetailPage {
    pub active_tab: &'static str,
    /// Sub-tab strip selector under the monitor header. `"monitor"` on
    /// this view; the Incidents page renders the same partial with
    /// `"incidents"`.
    pub subtab: &'static str,
    pub ongoing_count: usize,
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub interval_s: u64,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub last_status: &'static str,
    /// ISO 8601 timestamp of the most recent check, "" when none. Drives
    /// the client-side "checked Ns ago · next in Ns" ticker.
    pub last_at_iso: Arc<str>,
    pub uptime: Arc<UptimeStatsView>,
    pub results: Arc<[ResultRow]>,
    pub results_has_more: bool,
    pub config_json: String,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub range_base_path: String,
    pub from_iso: String,
    pub to_iso: String,
    pub from_human: String,
    pub to_human: String,
}

#[derive(Clone)]
pub struct UptimeStatsView {
    pub total: u64,
    pub up: u64,
    pub down: u64,
    pub degraded: u64,
    pub error: u64,
    pub uptime_pct: String,
}

impl From<UptimeStats> for UptimeStatsView {
    fn from(s: UptimeStats) -> Self {
        Self {
            total: s.total,
            up: s.up,
            down: s.down,
            degraded: s.degraded,
            error: s.error,
            uptime_pct: format!("{:.2}", s.uptime_pct),
        }
    }
}

/// ISO and human-readable strings for a `(from, to)` window. Both
/// detail-page templates render the same four fields in the chrome
/// (range pill caption, time inputs); precomputing once keeps the
/// template free of filter chains for one-shot values.
struct WindowLabels {
    from_iso: String,
    to_iso: String,
    from_human: String,
    to_human: String,
}

impl WindowLabels {
    fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        Self {
            from_iso: fmt_ts(from),
            to_iso: fmt_ts(to),
            from_human: fmt_human(from),
            to_human: fmt_human(to),
        }
    }
}

/// Snapshot of the per-target live region: uptime stats + recent rows +
/// last-seen status. Cached in `AppState::live_data_cache` for 5s; both
/// the full-page detail view and the htmx live-partial poll read from
/// it so a burst of either kind collapses to one CH round-trip. Inner
/// fields are `Arc` so a cache hit clones a pointer instead of the
/// full row vector + uptime struct per request.
pub struct LiveData {
    pub uptime: Arc<UptimeStatsView>,
    pub result_rows: Arc<[ResultRow]>,
    pub results_has_more: bool,
    pub last_status: &'static str,
    pub last_at_iso: Arc<str>,
}

/// Cached front door for [`load_live_data`]. Returns a moka-shared
/// `Arc<LiveData>` keyed on `(org, target_id, range_key)`. Preset
/// ranges are cached for 5s; ad-hoc `from`/`to` windows skip the cache
/// (one-off queries shouldn't pollute the shared bucket). Both the
/// full-page `index` and the htmx `live_partial` go through here so a
/// burst of either request type collapses to a single CH round-trip.
async fn load_live_data_cached(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
    range_key: &'static str,
    custom_from: Option<DateTime<Utc>>,
    custom_to: Option<DateTime<Utc>>,
) -> WebResult<Arc<LiveData>> {
    let cacheable = custom_from.is_none() && custom_to.is_none();
    if cacheable {
        let key = (org, target_id, range_key);
        if let Some(data) = state.live_data_cache.get(&key) {
            return Ok(data);
        }
        let (from, to) = resolve_window(range_key, custom_from, custom_to);
        let data = Arc::new(load_live_data(state, org, target_id, from, to).await?);
        state.live_data_cache.insert(key, data.clone());
        Ok(data)
    } else {
        let (from, to) = resolve_window(range_key, custom_from, custom_to);
        Ok(Arc::new(
            load_live_data(state, org, target_id, from, to).await?,
        ))
    }
}

/// Cheapest possible read of "what status is this monitor in right
/// now?" — one row from the last [`LAST_RESULT_WINDOW_DAYS`] days.
/// Used by the incidents tab so the header badge doesn't require the
/// full uptime + recent-results scan `load_live_data` performs.
async fn latest_status_probe(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
) -> WebResult<(&'static str, String)> {
    let to = Utc::now();
    let from = to - Duration::days(LAST_RESULT_WINDOW_DAYS);
    let row = state
        .results_store
        .list_results(org, target_id, TimeRange { from, to }, 1, 0)
        .await?
        .into_iter()
        .next();
    Ok((
        row.as_ref().map(|r| r.status.as_str()).unwrap_or(""),
        row.map(|r| fmt_ts(r.timestamp)).unwrap_or_default(),
    ))
}

// Shared "what does the live detail region show?" loader. The full-page
// `index` handler enriches it with chrome (range options, config JSON,
// header strings); the `live_partial` handler returns it as-is for htmx
// polling. Keeping the query graph in one place keeps the partial
// byte-identical to the section the full page renders.
async fn load_live_data(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> WebResult<LiveData> {
    let time_range = TimeRange { from, to };
    let (uptime, mut results) = tokio::try_join!(
        state.results_store.uptime(org, target_id, time_range),
        state
            .results_store
            .list_results(org, target_id, time_range, RESULTS_PAGE_LIMIT + 1, 0),
    )?;
    let results_has_more = results.len() > RESULTS_PAGE_LIMIT;
    if results_has_more {
        results.truncate(RESULTS_PAGE_LIMIT);
    }

    let latest_outside_window = if results.is_empty()
        && let Some(window) = wider_status_window(from, to)
    {
        state
            .results_store
            .list_results(org, target_id, window, 1, 0)
            .await?
            .into_iter()
            .next()
    } else {
        None
    };
    let latest_for_badge = results.first().or(latest_outside_window.as_ref());
    let last_status = latest_for_badge.map(|r| r.status.as_str()).unwrap_or("");
    let last_at_iso: Arc<str> = latest_for_badge
        .map(|r| fmt_ts(r.timestamp))
        .unwrap_or_default()
        .into();
    let result_rows: Arc<[ResultRow]> = results
        .into_iter()
        .map(ResultRow::from)
        .collect::<Vec<_>>()
        .into();

    Ok(LiveData {
        uptime: Arc::new(uptime.into()),
        result_rows,
        results_has_more,
        last_status,
        last_at_iso,
    })
}

pub async fn index(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<DetailParams>,
) -> WebResult<DetailPage> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "monitor not found"))?;

    let range_key = resolve_range_key(params.range.as_deref(), &RANGE_KEYS, DEFAULT_RANGE);
    let (from, to) = resolve_window(range_key, params.from, params.to);
    let labels = WindowLabels::new(from, to);
    let live =
        load_live_data_cached(&state, org, target.id, range_key, params.from, params.to).await?;
    let ongoing_count = ongoing_from_status(live.last_status);
    let config_json = serde_json::to_string_pretty(&target.check)
        .map_err(|e| AppError::Other(anyhow::anyhow!(e)))?;
    let (kind, address) = describe_check(&target.check);

    Ok(DetailPage {
        active_tab: "targets",
        subtab: SUBTAB_MONITOR,
        ongoing_count,
        id: target.id.to_string(),
        name: target.name,
        kind,
        address,
        interval_s: target.interval.as_secs(),
        enabled: target.enabled,
        tags: target.tags,
        last_status: live.last_status,
        last_at_iso: Arc::clone(&live.last_at_iso),
        uptime: Arc::clone(&live.uptime),
        results: Arc::clone(&live.result_rows),
        results_has_more: live.results_has_more,
        config_json,
        range: range_key,
        range_options: build_range_options(range_key, &RANGE_KEYS),
        range_base_path: format!("/targets/{}", target.id),
        from_iso: labels.from_iso,
        to_iso: labels.to_iso,
        from_human: labels.from_human,
        to_human: labels.to_human,
    })
}

/// htmx-polled fragment that re-renders the KPI cards + recent-results
/// table. Byte-identical to the section the full page initially served
/// so swap is invisible. Reads from `live_data_cache` (5s TTL); a
/// burst of pollers + the full-page handler share the same snapshot.
/// Custom `from`/`to` query params skip the cache.
pub async fn live_partial(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<DetailParams>,
) -> WebResult<Response> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "monitor not found"))?;

    let range_key = resolve_range_key(params.range.as_deref(), &RANGE_KEYS, DEFAULT_RANGE);
    let live =
        load_live_data_cached(&state, org, target.id, range_key, params.from, params.to).await?;

    let page = DetailLive {
        id: target.id.to_string(),
        name: target.name,
        range: range_key,
        uptime: Arc::clone(&live.uptime),
        results: Arc::clone(&live.result_rows),
        results_has_more: live.results_has_more,
        last_at_iso: Arc::clone(&live.last_at_iso),
    };
    let rendered = page
        .render()
        .map_err(|e| AppError::Other(anyhow::anyhow!(e)))?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // The htmx poll URL is time-relative ("range=24h" → window
            // relative to now). Browser cache would silently serve
            // stale rows even after the server's 5s TTL elapsed.
            (header::CACHE_CONTROL, "no-store"),
        ],
        rendered,
    )
        .into_response())
}

/// Maps a preset range key to a window. Explicit `from`/`to` query params
/// override the preset (custom range case). Covers every key from both
/// `RANGE_KEYS` and `INCIDENT_RANGE_KEYS` so the shared cached loader
/// can serve either tab without falling back to the default branch.
fn resolve_window(
    key: &'static str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let to = to.unwrap_or_else(Utc::now);
    let span = match key {
        "1h" => Duration::hours(1),
        "7d" => Duration::days(7),
        "30d" => Duration::days(30),
        "90d" => Duration::days(90),
        _ => Duration::hours(24),
    };
    (from.unwrap_or(to - span), to)
}

/// Fallback window for the "current health" badge when the user-picked
/// chart range is empty. Returns `None` when the user's range already
/// covers the last-result window so callers can skip a redundant query.
fn wider_status_window(from: DateTime<Utc>, to: DateTime<Utc>) -> Option<TimeRange> {
    let widened = to - Duration::days(LAST_RESULT_WINDOW_DAYS);
    if from <= widened {
        None
    } else {
        Some(TimeRange { from: widened, to })
    }
}

impl From<CheckResult> for ResultRow {
    fn from(r: CheckResult) -> Self {
        Self {
            timestamp: fmt_human(r.timestamp),
            status: r.status.as_str(),
            duration_ms: r.duration_ms,
            response_code: r.response_code.map(|c| c.to_string()).unwrap_or_default(),
            error: r.error.unwrap_or_default(),
        }
    }
}

impl From<Incident> for IncidentRow {
    fn from(inc: Incident) -> Self {
        let ongoing = inc.ended_at.is_none();
        let duration_secs = inc.closed_duration().map(|d| d.num_seconds());
        Self {
            id: inc.id,
            severity: inc.status.as_str(),
            started_at: inc.started_at,
            ended_at: inc.ended_at,
            duration_secs,
            check_count: inc.check_count,
            error_sample: inc.error_sample.unwrap_or_default(),
            ongoing,
        }
    }
}

pub async fn incidents(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<DetailParams>,
) -> WebResult<IncidentsPage> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "monitor not found"))?;

    let range_key = resolve_range_key(
        params.range.as_deref(),
        &INCIDENT_RANGE_KEYS,
        INCIDENT_DEFAULT_RANGE,
    );
    let (from, to) = resolve_incident_window(range_key, params.from, params.to);
    let time_range = TimeRange { from, to };

    let labels = WindowLabels::new(from, to);
    let interval = target.interval;
    // The incidents tab needs only the badge's `last_status` from the
    // live region — not the uptime stats or 60 recent rows. Probe one
    // row instead of running the full live loader; a 90d preset would
    // otherwise scan the entire window for the same single field.
    let ((last_status, last_at_iso), mut incidents) =
        tokio::try_join!(latest_status_probe(&state, org, target.id), async {
            state
                .results_store
                .list_incidents(
                    org,
                    target.id,
                    IncidentListQuery::page(time_range, interval, INCIDENTS_PAGE_LIMIT + 1),
                )
                .await
                .map_err(WebError::from)
        },)?;

    let incidents_has_more = incidents.len() > INCIDENTS_PAGE_LIMIT;
    if incidents_has_more {
        incidents.truncate(INCIDENTS_PAGE_LIMIT);
    }
    // Tab badge: prefer the live-status signal so the count matches
    // what the Monitor tab shows. Fall back to the list (e.g. user
    // narrowed to a window where last_status is stale) by counting
    // open runs the coalescer kept.
    let ongoing_count = ongoing_from_status(last_status)
        .max(incidents.iter().filter(|i| i.ended_at.is_none()).count());
    let (kind, address) = describe_check(&target.check);

    Ok(IncidentsPage {
        active_tab: "targets",
        subtab: SUBTAB_INCIDENTS,
        ongoing_count,
        id: target.id.to_string(),
        name: target.name,
        kind,
        address,
        interval_s: target.interval.as_secs(),
        enabled: target.enabled,
        tags: target.tags,
        last_status,
        last_at_iso,
        incidents: incidents.into_iter().map(IncidentRow::from).collect(),
        incidents_has_more,
        range: range_key,
        range_options: build_range_options(range_key, &INCIDENT_RANGE_KEYS),
        range_base_path: format!("/targets/{}/incidents", target.id),
        from_iso: labels.from_iso,
        to_iso: labels.to_iso,
        from_human: labels.from_human,
        to_human: labels.to_human,
    })
}

fn resolve_incident_window(
    key: &'static str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let to = to.unwrap_or_else(Utc::now);
    let span = match key {
        "24h" => Duration::hours(24),
        "7d" => Duration::days(7),
        "90d" => Duration::days(90),
        _ => Duration::days(30),
    };
    (from.unwrap_or(to - span), to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_page() -> DetailPage {
        DetailPage {
            active_tab: "targets",
            subtab: SUBTAB_MONITOR,
            ongoing_count: 0,
            id: "00000000-0000-0000-0000-000000000001".into(),
            name: "api".into(),
            kind: "HTTP",
            address: "https://example.com".into(),
            interval_s: 60,
            enabled: true,
            tags: vec!["prod".into()],
            last_status: "up",
            last_at_iso: Arc::from("2026-05-13T12:00:00Z"),
            uptime: Arc::new(UptimeStatsView {
                total: 100,
                up: 99,
                down: 1,
                degraded: 0,
                error: 0,
                uptime_pct: "99.00".into(),
            }),
            results: Arc::from(vec![ResultRow {
                timestamp: "2026-05-13T12:00:00Z".into(),
                status: "up",
                duration_ms: 42,
                response_code: "200".into(),
                error: String::new(),
            }]),
            results_has_more: false,
            config_json: r#"{"type":"http"}"#.into(),
            range: "24h",
            range_options: build_range_options("24h", &RANGE_KEYS),
            range_base_path: "/targets/00000000-0000-0000-0000-000000000001".into(),
            from_iso: "2026-05-12T12:00:00Z".into(),
            to_iso: "2026-05-13T12:00:00Z".into(),
            from_human: "2026-05-12 12:00 UTC".into(),
            to_human: "2026-05-13 12:00 UTC".into(),
        }
    }

    #[test]
    fn detail_renders_header_and_widgets() {
        let html = sample_page().render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("api"));
        assert!(html.contains("Uptime"));
        assert!(html.contains("99.00"));
        assert!(html.contains("data-endpoint"));
        assert!(html.contains("/api/v1/targets/00000000-0000-0000-0000-000000000001/results"));
    }

    #[test]
    fn detail_delete_uses_shared_confirm_modal_not_browser_dialog() {
        let html = sample_page().render().unwrap();
        assert!(html.contains("data-confirm-modal"));
        assert!(html.contains(r#"data-confirm-title="Delete monitor?""#));
        assert!(html.contains("data-confirm-danger"));
        assert!(!html.contains("hx-confirm"));
    }

    #[test]
    fn range_options_mark_active() {
        let opts = build_range_options("7d", &RANGE_KEYS);
        assert!(opts.iter().any(|o| o.key == "7d" && o.selected));
        assert_eq!(opts.iter().filter(|o| o.selected).count(), 1);
    }

    #[test]
    fn wider_status_window_returns_some_for_short_user_range() {
        let to = DateTime::parse_from_rfc3339("2026-05-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let win = wider_status_window(to - Duration::hours(1), to).expect("widen");
        assert_eq!(win.from, to - Duration::days(LAST_RESULT_WINDOW_DAYS));
        assert_eq!(win.to, to);
    }

    #[test]
    fn wider_status_window_returns_none_for_wide_user_range() {
        let to = DateTime::parse_from_rfc3339("2026-05-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(wider_status_window(to - Duration::days(30), to).is_none());
        assert!(wider_status_window(to - Duration::days(LAST_RESULT_WINDOW_DAYS), to).is_none());
    }

    fn sample_live() -> DetailLive {
        DetailLive {
            id: "00000000-0000-0000-0000-000000000001".into(),
            name: "api".into(),
            range: "24h",
            uptime: Arc::new(UptimeStatsView {
                total: 100,
                up: 99,
                down: 1,
                degraded: 0,
                error: 0,
                uptime_pct: "99.00".into(),
            }),
            results: Arc::from(Vec::<ResultRow>::new()),
            results_has_more: false,
            last_at_iso: Arc::from("2026-05-13T12:00:00Z"),
        }
    }

    #[test]
    fn live_partial_renders_kpi_swap_target_plus_oob_tbody() {
        let html = sample_live().render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(html.contains(r#"id="detail-live-kpi""#));
        assert!(html.contains(
            "hx-get=\"/web/partials/targets/00000000-0000-0000-0000-000000000001/live?range=24h\""
        ));
        assert!(html.contains(r#"hx-trigger="every 60s, sm:refresh-live from:body""#));
        assert!(html.contains(r#"hx-swap="outerHTML""#));
        assert!(html.contains(r#"data-newest-ts="2026-05-13T12:00:00Z""#));
        // Tbody MUST be wrapped in <template> so browsers don't strip
        // it as orphan-of-table during response parsing.
        assert!(html.contains("<template>"));
        let template_open = html.find("<template>").unwrap();
        let recent_id = html.find(r#"id="detail-live-recent""#).unwrap();
        let template_close = html.find("</template>").unwrap();
        assert!(
            template_open < recent_id && recent_id < template_close,
            "OOB tbody must live inside the <template> wrapper"
        );
        assert!(html.contains(r#"hx-swap-oob="true""#));
        assert!(html.contains("99.00"));
    }

    #[test]
    fn detail_page_wraps_kpi_and_recent_separately_with_charts_between() {
        let html = sample_page().render().unwrap();
        assert!(html.contains(r#"id="detail-live-kpi""#));
        assert!(html.contains(r#"id="detail-live-recent""#));
        assert!(html.contains(r#"id="latency-chart""#));
        let kpi_pos = html.find(r#"id="detail-live-kpi""#).expect("kpi present");
        let chart_pos = html.find(r#"id="latency-chart""#).expect("chart present");
        let recent_pos = html
            .find(r#"id="detail-live-recent""#)
            .expect("recent present");
        assert!(kpi_pos < chart_pos, "KPI must render before charts");
        assert!(
            chart_pos < recent_pos,
            "Charts must render before Recent results"
        );
    }

    #[test]
    fn resolve_range_key_clamps_to_allowed() {
        assert_eq!(
            resolve_range_key(Some("1h"), &RANGE_KEYS, DEFAULT_RANGE),
            "1h"
        );
        assert_eq!(
            resolve_range_key(Some("garbage"), &RANGE_KEYS, DEFAULT_RANGE),
            "24h"
        );
        assert_eq!(resolve_range_key(None, &RANGE_KEYS, DEFAULT_RANGE), "24h");
    }

    #[test]
    fn resolve_incident_range_key_defaults_to_30d() {
        let k = |s| resolve_range_key(s, &INCIDENT_RANGE_KEYS, INCIDENT_DEFAULT_RANGE);
        assert_eq!(k(None), "30d");
        assert_eq!(k(Some("")), "30d");
        assert_eq!(k(Some("garbage")), "30d");
        assert_eq!(k(Some("24h")), "24h");
        assert_eq!(k(Some("90d")), "90d");
    }

    #[test]
    fn incident_row_falls_back_to_start_end_when_duration_secs_missing() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2026, 5, 12, 8, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 5, 12, 8, 7, 0).unwrap();
        let inc = crate::domain::Incident {
            id: Uuid::nil(),
            target_id: Uuid::nil(),
            started_at: start,
            ended_at: Some(end),
            status: crate::domain::CheckStatus::Down,
            duration_secs: None,
            check_count: 7,
            error_sample: None,
            severity: Default::default(),
            public_title: None,
            public_description: None,
            created_at: None,
            updated_at: None,
            updates: Vec::new(),
        };
        let row = IncidentRow::from(inc);
        assert!(!row.ongoing);
        assert_eq!(row.duration_secs, Some(7 * 60));
    }

    fn sample_incidents_page(incidents: Vec<IncidentRow>, ongoing_count: usize) -> IncidentsPage {
        IncidentsPage {
            active_tab: "targets",
            subtab: SUBTAB_INCIDENTS,
            ongoing_count,
            id: "00000000-0000-0000-0000-000000000001".into(),
            name: "api".into(),
            kind: "HTTP",
            address: "https://example.com".into(),
            interval_s: 60,
            enabled: true,
            tags: vec!["prod".into()],
            last_status: "down",
            last_at_iso: "2026-05-13T12:00:00Z".into(),
            incidents,
            incidents_has_more: false,
            range: "30d",
            range_options: build_range_options("30d", &INCIDENT_RANGE_KEYS),
            range_base_path: "/targets/00000000-0000-0000-0000-000000000001/incidents".into(),
            from_iso: "2026-04-13T12:00:00Z".into(),
            to_iso: "2026-05-13T12:00:00Z".into(),
            from_human: "2026-04-13 12:00 UTC".into(),
            to_human: "2026-05-13 12:00 UTC".into(),
        }
    }

    fn ongoing_row() -> IncidentRow {
        use chrono::TimeZone;
        IncidentRow {
            id: Uuid::from_u128(0x0000_0001),
            severity: "down",
            started_at: Utc.with_ymd_and_hms(2026, 5, 13, 11, 50, 0).unwrap(),
            ended_at: None,
            duration_secs: None,
            check_count: 4,
            error_sample: "connection refused".into(),
            ongoing: true,
        }
    }

    fn resolved_row() -> IncidentRow {
        use chrono::TimeZone;
        IncidentRow {
            id: Uuid::from_u128(0x0000_0002),
            severity: "down",
            started_at: Utc.with_ymd_and_hms(2026, 5, 12, 8, 0, 0).unwrap(),
            ended_at: Some(Utc.with_ymd_and_hms(2026, 5, 12, 8, 7, 0).unwrap()),
            duration_secs: Some(420),
            check_count: 7,
            error_sample: "HTTP 503 Service Unavailable".into(),
            ongoing: false,
        }
    }

    #[test]
    fn incidents_page_renders_empty_state_when_no_incidents() {
        let html = sample_incidents_page(vec![], 0).render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("No incidents in the last 30d"));
        assert!(!html.contains("<table"));
        assert!(html.contains("aria-current=\"page\""));
    }

    #[test]
    fn incidents_page_renders_table_rows_with_ongoing_emphasis() {
        let html = sample_incidents_page(vec![ongoing_row(), resolved_row()], 1)
            .render()
            .unwrap();
        assert!(html.contains("<table"));
        // Ongoing emphasis: red left border + pulsing badge + severity-tagged label.
        assert!(html.contains("border-l-rose-500"));
        assert!(html.contains("animate-pulse"));
        assert!(html.contains("Ongoing · down"));
        // Resolved row uses the regular severity badge.
        assert!(html.contains(r#"status-badge status-badge--down">down<"#));
        // Each row has a hidden detail row + the chevron for expand.
        assert!(html.contains("data-incident-detail"));
        assert!(html.contains("data-incident-chevron"));
        // Row carries the window data the JS uses to fetch the timeline.
        assert!(html.contains(r#"data-from="2026-05-13T11:50:00Z""#));
    }

    #[test]
    fn incidents_page_ongoing_badge_appears_on_tab_strip() {
        let html = sample_incidents_page(vec![ongoing_row()], 1)
            .render()
            .unwrap();
        assert!(html.contains(r#"id="tab-incidents-badge""#));
        assert!(html.contains(r#"aria-label="1 ongoing">1<"#));
    }

    #[test]
    fn incidents_page_omits_tab_badge_when_no_ongoing() {
        let html = sample_incidents_page(vec![resolved_row()], 0)
            .render()
            .unwrap();
        assert!(!html.contains(r#"id="tab-incidents-badge""#));
    }

    #[test]
    fn detail_page_tab_strip_marks_monitor_subtab_active() {
        let html = sample_page().render().unwrap();
        // Both tabs link to their own paths.
        assert!(html.contains(r#"href="/targets/00000000-0000-0000-0000-000000000001""#));
        assert!(html.contains(r#"href="/targets/00000000-0000-0000-0000-000000000001/incidents""#));
        // The Monitor anchor must carry aria-current; the Incidents one must not.
        let monitor_href = r#"href="/targets/00000000-0000-0000-0000-000000000001""#;
        let monitor_pos = html.find(monitor_href).expect("monitor link present");
        let monitor_anchor_end = html[monitor_pos..]
            .find("</a>")
            .expect("monitor anchor terminator");
        let monitor_anchor = &html[monitor_pos..monitor_pos + monitor_anchor_end];
        assert!(monitor_anchor.contains("aria-current=\"page\""));
    }

    #[test]
    fn incidents_page_subtab_active_is_incidents() {
        let html = sample_incidents_page(vec![], 0).render().unwrap();
        let incidents_href = r#"href="/targets/00000000-0000-0000-0000-000000000001/incidents""#;
        let pos = html.find(incidents_href).expect("incidents link present");
        let anchor_end = html[pos..].find("</a>").expect("anchor terminator");
        let anchor = &html[pos..pos + anchor_end];
        assert!(anchor.contains("aria-current=\"page\""));
    }
}
