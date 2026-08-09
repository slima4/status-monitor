//! Operator dashboard — V3 landing page. Replaces the donut+bar layout
//! with a dense per-monitor table backed by ONE batched ClickHouse
//! rollup + ONE batched 60-min sparkline query (both cached 5 s per
//! `(org, range)`). Scales to 1k+ monitors per org without N
//! round-trips per render.
//!
//! Hosts the `/` dispatcher too: a per-org status subdomain serves the
//! public status page, every other host falls through to the operator
//! dashboard. The branch lives here rather than as router-level
//! middleware because axum's `Router::layer` runs *after* path matching
//! — rewriting the URI in a layer would never re-route to `/status`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::api::types::{
    DashboardMetrics, DashboardSparkBucket, FleetRibbonBucket, PriorPeriodSummary,
};
use crate::app::AppState;
use crate::domain::{IncidentSeverity, OrgId, uptime_pct_from_downtime};
use crate::storage::{IncidentBrief, IncidentBriefFilter, TargetFilter, TimeRange};
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::host::is_subdomain_public_request;
use crate::web::views::public_status::{self, StatusParams};
use crate::web::views::region_display::{LabeledRegion, labeled_regions};
use crate::web::views::{
    HumanDur, RangeOption, build_range_options, describe_check, resolve_range_key,
};
use crate::web::{AuthedBrowser, CurrentOrg, CurrentUser};

pub(crate) const RANGE_KEYS: [&str; 4] = ["24h", "7d", "30d", "90d"];
pub(crate) const DEFAULT_RANGE: &str = "24h";
pub(crate) const STATUS_FILTERS: [&str; 5] = ["any", "up", "degraded", "down", "paused"];
pub(crate) const TYPE_FILTERS: [&str; 9] = [
    "any",
    "http",
    "tcp",
    "ping",
    "heartbeat",
    "dns",
    "tls",
    "domain",
    "flow",
];
pub(crate) const FILTER_ANY: &str = "any";
/// Fixed sparkline window — "right-now" trend, decoupled from the
/// selected rollup range so an operator browsing 90 d still sees a
/// fresh 1 h trace per monitor.
const SPARK_MINUTES: i64 = 60;
const SPARK_BUCKETS: usize = SPARK_MINUTES as usize;
/// Fixed fleet ribbon: 24 h split into 48 × 30-minute cells. Window is
/// independent of the selected range so the ribbon always reads as
/// "last 24 h fleet health" regardless of the table's rollup span.
const RIBBON_HOURS: i64 = 24;
const RIBBON_BUCKETS: usize = 48;
const RIBBON_BUCKET_SECONDS: u32 = (RIBBON_HOURS as u32 * 3600) / RIBBON_BUCKETS as u32;
/// Names shown in a cell tooltip before "+N more"; the drill has the full set.
const DOWN_PREVIEW: usize = 6;
const _: () = assert!(
    (RIBBON_HOURS as u32 * 3600).is_multiple_of(RIBBON_BUCKETS as u32),
    "RIBBON_BUCKETS must divide the 24h window evenly so every second maps to a slot",
);
/// Cap rows on a single page render. Beyond this an org would benefit
/// from search/filter on `/targets`; rendering 5 k rows inline is a
/// browser-side hazard, not just a server cost.
const ROW_LIMIT: usize = 500;

#[derive(Debug, Default, Deserialize)]
pub struct DashboardParams {
    #[serde(default)]
    pub range: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Bucket start (unix-seconds) of a clicked ribbon cell; filters the table
    /// to the monitors that dipped in that window.
    #[serde(default)]
    pub down_at: Option<i64>,
    /// Login flows set `joined=<slug>` after auto-accepting an invitation. The
    /// slug is validated against the active org before the banner renders, so
    /// unlike the one-shot flash signals it stays a (harmless) query param.
    #[serde(default)]
    pub joined: Option<String>,
}

/// One row in the dashboard table — every cell the template renders.
#[derive(Clone)]
pub struct DashboardRow {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub enabled: bool,
    /// "up" / "down" / "degraded" / "error" / "" — empty when the
    /// monitor has never reported in the selected range. Drives both
    /// the `dashboard-dot--*` colour and the `data-status` row attribute.
    pub last_status: &'static str,
    /// Median latency in ms, formatted (e.g. "142 ms"). "—" when no
    /// samples.
    pub p50_label: String,
    pub p95_label: String,
    /// `0.0..100.0` error rate (`100 - uptime%`). Displayed with one
    /// decimal so a 0.03 % flake doesn't round to 0.
    pub err_pct_label: String,
    pub uptime_pct_label: String,
    pub samples: u64,
    /// `60` minute-aligned average-duration points, oldest → newest.
    /// Empty buckets carry `None`; the renderer skips them and connects
    /// the line across the gap rather than drawing a drop-to-zero spike.
    pub spark: Vec<Option<f32>>,
    /// Pre-rendered SVG path `d` for the sparkline polyline. Built
    /// once server-side so the template stays static markup and the
    /// browser does no per-row math on render.
    pub spark_path: String,
    /// SVG `<path d=…>` for the soft area fill under the line — the line
    /// closed down to the baseline. Empty below two samples.
    pub spark_fill: String,
    pub spark_baseline_y: u32,
}

/// What [`load_snapshot`] returns. Held in `AppState::dashboard_cache`
/// behind an `Arc` so cache hits are pointer-bumps even when an org has
/// hundreds of monitors. Inner `Arc`s let the per-page template clone
/// fields out for the surrounding chrome without re-cloning the table.
#[derive(Clone)]
pub struct DashboardSnapshot {
    pub rows: Arc<[DashboardRow]>,
    pub kpi_cards: Arc<[KpiCardSpec]>,
    pub matches: usize,
    pub truncated: bool,
    pub active_incidents: Arc<[DashboardActiveIncident]>,
    pub status_counts: StatusCounts,
    pub type_counts: Arc<[TypeCount]>,
    pub ribbon: FleetRibbon,
}

#[derive(Clone)]
pub struct DashboardKpis {
    pub uptime_pct_label: String,
    pub avg_response_ms_label: String,
    pub checks_label: String,
    pub checks_successful_label: String,
    pub incidents: u64,
}

/// Per-card config passed to the KPI partial — kills the 3-way copy that
/// a hand-unrolled template would force.
#[derive(Clone)]
pub struct KpiCardSpec {
    pub label: String,
    pub value: String,
    pub hint_html: String,
    /// `None` when there is no prior data — template skips the line.
    pub delta: Option<KpiDelta>,
    pub spark_tint: &'static str,
    /// The card's own metric over time. Each card plots what it counts.
    pub spark_path: String,
    pub spark_fill: String,
}

/// Renderable Δ-vs-prior chip: `<span class="{class}">{arrow} {body} vs
/// prior</span>` — wrapper lives in the template so no `<span>` strings
/// are allocated per render.
#[derive(Clone)]
pub struct KpiDelta {
    pub class: &'static str,
    pub arrow: &'static str,
    pub body: String,
}

/// Health-rail counts. A disabled monitor is "paused" regardless of its
/// last sample; everything else flows from `last_status`.
#[derive(Clone, Copy, Default)]
pub struct StatusCounts {
    pub up: u32,
    pub degraded: u32,
    pub down: u32,
    pub paused: u32,
}

/// One cell of the 48-seg fleet ribbon. A non-empty `down_targets` makes the
/// cell a drill link (`bucket_ts` is its `down_at` value); `down_preview` caps
/// the tooltip's names while the drill keeps the full set.
#[derive(Clone)]
pub struct FleetRibbonSeg {
    pub class: &'static str,
    pub time: String,
    pub stat: String,
    pub down_preview: Arc<[String]>,
    pub bucket_ts: i64,
    pub down_targets: Arc<[Uuid]>,
}

#[derive(Clone)]
pub struct FleetRibbon {
    pub segs: Arc<[FleetRibbonSeg]>,
    /// Aggregate uptime label across the full 24h window ("99.71%" or "—").
    pub uptime_label: String,
}

#[derive(Clone)]
pub struct TypeCount {
    pub label: &'static str,
    pub count: u32,
    pub active: bool,
}

#[derive(Clone)]
pub struct DashboardActiveIncident {
    pub id: String,
    pub target_id: String,
    pub title: String,
    pub age_label: String,
    pub severity_label: &'static str,
    pub severity_class: &'static str,
    pub latest_update: Option<DashboardIncidentUpdate>,
}

#[derive(Clone)]
pub struct DashboardIncidentUpdate {
    pub posted_at: chrono::DateTime<chrono::Utc>,
    pub phase_label: &'static str,
    pub message: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub active_tab: &'static str,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub kpi_cards: Arc<[KpiCardSpec]>,
    pub rows: Arc<[DashboardRow]>,
    pub matches: usize,
    pub truncated: bool,
    pub onboarding: bool,
    pub active_incidents: Arc<[DashboardActiveIncident]>,
    pub status_counts: StatusCounts,
    pub type_counts: Arc<[TypeCount]>,
    pub ribbon: FleetRibbon,
    pub regions: Vec<LabeledRegion>,
    pub selected_region: Option<String>,
    pub status_options: Vec<RangeOption>,
    pub selected_status: Option<&'static str>,
    pub selected_kind: Option<&'static str>,
    pub drill: Option<DrillChip>,
    pub restored_notice: bool,
    pub joined_notice: Option<String>,
    pub invite_missed_notice: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/table.html")]
pub struct DashboardTablePartial {
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub kpi_cards: Arc<[KpiCardSpec]>,
    pub rows: Arc<[DashboardRow]>,
    pub matches: usize,
    pub truncated: bool,
    pub active_incidents: Arc<[DashboardActiveIncident]>,
    pub status_counts: StatusCounts,
    pub type_counts: Arc<[TypeCount]>,
    pub ribbon: FleetRibbon,
    pub regions: Vec<LabeledRegion>,
    pub selected_region: Option<String>,
    pub status_options: Vec<RangeOption>,
    pub selected_status: Option<&'static str>,
    pub selected_kind: Option<&'static str>,
    pub drill: Option<DrillChip>,
}

pub async fn root(state: State<AppState>, mut parts: Parts) -> Response {
    let State(ref app_state) = state;
    if is_subdomain_public_request(app_state, &parts.headers) {
        // Preserve axum's standard `Query<T>` rejection — a malformed
        // `?fragment=` value used to 400 via the framework extractor, so a
        // bare `.unwrap_or(default)` here would silently turn invalid params
        // into a 200.
        let query = match Query::<StatusParams>::try_from_uri(&parts.uri) {
            Ok(q) => q,
            Err(rej) => return rej.into_response(),
        };
        return public_status::index(state, parts.headers, query).await;
    }
    // Operator dashboard. Re-run the extractors the routed `index`
    // would have run so rejection paths (login redirect, org error
    // envelope) stay byte-identical to a direct mount.
    let auth = match AuthedBrowser::from_request_parts(&mut parts, app_state).await {
        Ok(a) => a,
        Err(rej) => return rej.into_response(),
    };
    let org = match CurrentOrg::from_request_parts(&mut parts, app_state).await {
        Ok(o) => o,
        Err(rej) => return rej.into_response(),
    };
    let user = match CurrentUser::from_request_parts(&mut parts, app_state).await {
        Ok(u) => u,
        Err(rej) => return rej.into_response(),
    };
    let params = match Query::<DashboardParams>::try_from_uri(&parts.uri) {
        Ok(q) => q,
        Err(rej) => return rej.into_response(),
    };
    let cookies = match Cookies::from_request_parts(&mut parts, app_state).await {
        Ok(c) => c,
        Err(rej) => return rej.into_response(),
    };
    match index(auth, state, org, user, cookies, params).await {
        Ok(page) => page.into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn index(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    org: CurrentOrg,
    _user: CurrentUser,
    cookies: Cookies,
    Query(params): Query<DashboardParams>,
) -> WebResult<DashboardPage> {
    // One-shot post-login banners ride a flash cookie (consumed here) rather
    // than spoofable query params.
    let flash = crate::web::flash::take(&cookies, &state.cfg.auth.session.cookie_domain);
    let range = resolve_range_key(params.range.as_deref(), &RANGE_KEYS, DEFAULT_RANGE);
    let status = resolve_range_key(params.status.as_deref(), &STATUS_FILTERS, FILTER_ANY);
    let selected_status = (status != FILTER_ANY).then_some(status);
    let kind = resolve_range_key(params.kind.as_deref(), &TYPE_FILTERS, FILTER_ANY);
    let selected_kind = (kind != FILTER_ANY).then_some(kind);
    let region_ids = state.regions_for_org(org.0).await?;
    let selected_region = resolve_region(params.region, &region_ids);
    let snapshot = snapshot_for(&state, org.0, range, selected_region.as_deref()).await?;
    let catalog = state.regions_detailed().await?;
    let regions = labeled_regions(&catalog, region_ids);
    let onboarding = snapshot.matches == 0;
    let drill = params.down_at.and_then(|ts| resolve_drill(&snapshot, ts));
    let (rows, matches) = filter_rows(&snapshot, selected_status, selected_kind, drill.as_ref());
    // Banner only when the slug matches the ACTIVE org — a pasted/crafted
    // ?joined= for some other org renders nothing.
    let org_row = match state.db.as_ref() {
        Some(pool) if params.joined.is_some() => crate::storage::orgs::get_org(pool, org.0).await?,
        _ => None,
    };
    let joined_notice = params.joined.as_deref().and_then(|slug| {
        org_row
            .as_ref()
            .filter(|o| o.slug == slug)
            .map(|o| o.name.clone())
    });
    Ok(DashboardPage {
        active_tab: "dashboard",
        range,
        range_options: build_range_options(range, &RANGE_KEYS),
        kpi_cards: Arc::clone(&snapshot.kpi_cards),
        rows,
        matches,
        truncated: snapshot.truncated,
        onboarding,
        active_incidents: Arc::clone(&snapshot.active_incidents),
        status_counts: snapshot.status_counts,
        type_counts: type_chips(&snapshot, selected_status, selected_kind),
        ribbon: snapshot.ribbon.clone(),
        regions,
        selected_region,
        status_options: build_range_options(status, &STATUS_FILTERS),
        selected_status,
        selected_kind,
        drill: drill.map(|d| d.chip),
        restored_notice: flash.restored,
        joined_notice,
        invite_missed_notice: flash.invite_missed,
    })
}

/// An unknown region collapses to the all-regions view, not an empty dashboard.
pub(crate) fn resolve_region(requested: Option<String>, regions: &[String]) -> Option<String> {
    requested.filter(|r| regions.iter().any(|x| x == r))
}

/// The active ribbon-cell drill, surfaced to the template for the poll URL and
/// the clear chip. One field so the two can't drift out of lockstep.
#[derive(Clone)]
pub struct DrillChip {
    pub down_at: i64,
    pub label: String,
}

/// A ribbon-cell drill resolved against the cached snapshot: the chip plus the
/// set of monitors that dipped in the clicked window. `None` when the bucket
/// has aged out of the 24h ribbon or never had a dip.
struct Drill {
    chip: DrillChip,
    down: std::collections::HashSet<String>,
}

fn resolve_drill(snapshot: &DashboardSnapshot, down_at: i64) -> Option<Drill> {
    let seg = snapshot
        .ribbon
        .segs
        .iter()
        .find(|s| s.bucket_ts == down_at && !s.down_targets.is_empty())?;
    let label = DateTime::<Utc>::from_timestamp(down_at, 0)?
        .format("%H:%M")
        .to_string();
    Some(Drill {
        chip: DrillChip { down_at, label },
        down: seg.down_targets.iter().map(Uuid::to_string).collect(),
    })
}

/// Status, type and the ribbon drill are post-filters over the (cached)
/// snapshot: the table and its match count narrow while the KPI cards, health
/// rail, ribbon and chip counts stay fleet-wide, so the breakdown stays visible.
fn filter_rows(
    snapshot: &DashboardSnapshot,
    status: Option<&'static str>,
    kind: Option<&'static str>,
    drill: Option<&Drill>,
) -> (Arc<[DashboardRow]>, usize) {
    if status.is_none() && kind.is_none() && drill.is_none() {
        return (Arc::clone(&snapshot.rows), snapshot.matches);
    }
    let rows: Vec<DashboardRow> = snapshot
        .rows
        .iter()
        .filter(|r| status.is_none_or(|s| row_matches_status(r, s)))
        .filter(|r| kind.is_none_or(|k| r.kind.eq_ignore_ascii_case(k)))
        .filter(|r| drill.is_none_or(|d| d.down.contains(&r.id)))
        .cloned()
        .collect();
    let matches = rows.len();
    (Arc::from(rows.into_boxed_slice()), matches)
}

/// Chip strip for this request — the snapshot is cached across requests,
/// so neither `active` nor status-filtered counts can live there. Kinds
/// come from the fleet snapshot so the strip stays stable (a chip drops
/// to 0 under a status filter instead of vanishing while selected).
fn type_chips(
    snapshot: &DashboardSnapshot,
    status: Option<&'static str>,
    kind: Option<&'static str>,
) -> Arc<[TypeCount]> {
    if status.is_none() && kind.is_none() {
        // Unfiltered poll is the hot path — snapshot counts and the
        // "All" active flag are already correct, keep it a pointer bump.
        return Arc::clone(&snapshot.type_counts);
    }
    let count_for = |label: &'static str| {
        snapshot
            .rows
            .iter()
            .filter(|r| status.is_none_or(|s| row_matches_status(r, s)))
            .filter(|r| label == "All" || r.kind.eq_ignore_ascii_case(label))
            .count() as u32
    };
    let chips: Vec<TypeCount> = snapshot
        .type_counts
        .iter()
        .map(|c| TypeCount {
            label: c.label,
            count: match status {
                None => c.count,
                Some(_) => count_for(c.label),
            },
            active: match kind {
                None => c.label == "All",
                Some(k) => c.label.eq_ignore_ascii_case(k),
            },
        })
        .collect();
    Arc::from(chips.into_boxed_slice())
}

/// Mirrors `tally_status` bucketing so the filter agrees with the rail.
fn row_matches_status(row: &DashboardRow, status: &str) -> bool {
    match status {
        "paused" => !row.enabled,
        "down" => row.enabled && matches!(row.last_status, "down" | "error"),
        _ => row.enabled && row.last_status == status,
    }
}

/// All-regions is cached; a region-filtered view is built directly (selection is
/// rare, not worth widening the cache key).
async fn snapshot_for(
    state: &AppState,
    org: OrgId,
    range: &'static str,
    region: Option<&str>,
) -> WebResult<Arc<DashboardSnapshot>> {
    match region {
        Some(r) => Ok(Arc::new(build_snapshot(state, org, range, Some(r)).await?)),
        None => load_snapshot(state, org, range).await,
    }
}

/// htmx partial — the range-tab strip swaps just this fragment so a tab
/// click costs one CH query (cached) and a ~10 KB body, not a full
/// page. Returns `no-store` because the snapshot is time-relative.
pub async fn table_partial(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    org: CurrentOrg,
    Query(params): Query<DashboardParams>,
) -> WebResult<Response> {
    let range = resolve_range_key(params.range.as_deref(), &RANGE_KEYS, DEFAULT_RANGE);
    let status = resolve_range_key(params.status.as_deref(), &STATUS_FILTERS, FILTER_ANY);
    let selected_status = (status != FILTER_ANY).then_some(status);
    let kind = resolve_range_key(params.kind.as_deref(), &TYPE_FILTERS, FILTER_ANY);
    let selected_kind = (kind != FILTER_ANY).then_some(kind);
    let region_ids = state.regions_for_org(org.0).await?;
    let selected_region = resolve_region(params.region, &region_ids);
    let snapshot = snapshot_for(&state, org.0, range, selected_region.as_deref()).await?;
    let catalog = state.regions_detailed().await?;
    let regions = labeled_regions(&catalog, region_ids);
    let drill = params.down_at.and_then(|ts| resolve_drill(&snapshot, ts));
    let (rows, matches) = filter_rows(&snapshot, selected_status, selected_kind, drill.as_ref());
    let partial = DashboardTablePartial {
        range,
        range_options: build_range_options(range, &RANGE_KEYS),
        kpi_cards: Arc::clone(&snapshot.kpi_cards),
        rows,
        matches,
        truncated: snapshot.truncated,
        active_incidents: Arc::clone(&snapshot.active_incidents),
        status_counts: snapshot.status_counts,
        type_counts: type_chips(&snapshot, selected_status, selected_kind),
        ribbon: snapshot.ribbon.clone(),
        regions,
        selected_region,
        status_options: build_range_options(status, &STATUS_FILTERS),
        selected_status,
        selected_kind,
        drill: drill.map(|d| d.chip),
    };
    let rendered = partial.render().map_err(|e| {
        crate::web::error::WebError::from(crate::error::AppError::Other(anyhow::anyhow!(e)))
    })?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        rendered,
    )
        .into_response())
}

/// Cached front door — both `index` and `table_partial` reach the same
/// `Arc<DashboardSnapshot>` so a tab-spam burst collapses to one CH
/// round-trip. The cache itself enforces the 5 s TTL.
async fn load_snapshot(
    state: &AppState,
    org: OrgId,
    range: &'static str,
) -> WebResult<Arc<DashboardSnapshot>> {
    if let Some(snap) = state.dashboard_page_cache.get(&(org, range)) {
        return Ok(snap);
    }
    let snap = Arc::new(build_snapshot(state, org, range, None).await?);
    state
        .dashboard_page_cache
        .insert((org, range), Arc::clone(&snap));
    Ok(snap)
}

async fn build_snapshot(
    state: &AppState,
    org: OrgId,
    range: &'static str,
    region: Option<&str>,
) -> WebResult<DashboardSnapshot> {
    let to = Utc::now();
    let from = to - range_span(range);
    let time_range = TimeRange { from, to };
    let spark_from = to - Duration::minutes(SPARK_MINUTES);
    // Snap to a bucket boundary so the CH-side `toStartOfInterval` grid
    // aligns 1:1 with the labels we render. Without this `from = now -
    // 24h` is mid-bucket and the tooltips drift by up to 29 minutes.
    let ribbon_from = snap_to_bucket(to - Duration::hours(RIBBON_HOURS), RIBBON_BUCKET_SECONDS);

    // Region view lists only targets that run there — otherwise off-region
    // monitors fill the page (showing "—") and the ROW_LIMIT truncates the
    // wrong set. All-regions (region None) keeps the full list.
    let target_filter = TargetFilter {
        limit: Some(ROW_LIMIT + 1),
        offset: 0,
        region: region.map(str::to_owned),
        ..Default::default()
    };

    let (
        mut targets,
        rollup,
        spark_rows,
        (checks_total, checks_up, avg_ms_current, incidents),
        active_raw,
        ribbon_rows,
        prior,
        downtime_by_target,
    ) = tokio::try_join!(
        state.target_store.list(org, target_filter),
        state
            .results_store
            .dashboard_rollup(org, time_range, region),
        state
            .results_store
            .dashboard_sparkline(org, spark_from, to, region),
        state.results_store.last_n_summary(org, time_range, region),
        state.incident_narration_store.list_briefs(
            org,
            IncidentBriefFilter {
                oldest_first: true,
                limit: ACTIVE_INCIDENTS_LIMIT,
                ..Default::default()
            },
        ),
        state
            .results_store
            .fleet_ribbon(org, ribbon_from, to, RIBBON_BUCKET_SECONDS, region),
        state
            .results_store
            .prior_period_summary(org, time_range, region),
        state
            .incident_narration_store
            .confirmed_downtime_by_target(org, time_range),
    )?;

    let window_secs = (time_range.to - time_range.from).num_seconds();
    // A region filter keeps the raw per-region rate; only all-regions is confirmed.
    let confirmed = region.is_none();

    let truncated = targets.len() > ROW_LIMIT;
    if truncated {
        targets.truncate(ROW_LIMIT);
    }

    // Only dipped monitors are named in the ribbon tooltip, so clone names for
    // those ids alone rather than the whole (healthy) fleet on every build.
    let down_ids: std::collections::HashSet<Uuid> = ribbon_rows
        .iter()
        .flat_map(|r| r.down_targets.iter().copied())
        .collect();
    let target_names: HashMap<Uuid, String> = targets
        .iter()
        .filter(|t| down_ids.contains(&t.id))
        .map(|t| (t.id, t.name.clone()))
        .collect();
    let metrics_by_target: HashMap<Uuid, DashboardMetrics> =
        rollup.into_iter().map(|m| (m.target_id, m)).collect();
    let spark_by_target = group_sparks(&spark_rows, spark_from);
    // Cosmetic overlay — a silence-query hiccup must not fail the dashboard.
    let silenced: std::collections::HashSet<Uuid> = state
        .silence_store
        .open_target_ids(org)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut status_counts = StatusCounts::default();
    let mut type_acc: [u32; TYPE_CHIP_ORDER.len()] = [0; TYPE_CHIP_ORDER.len()];
    let rows: Vec<DashboardRow> = targets
        .into_iter()
        .map(|t| {
            let (kind, address) = describe_check(&t.check);
            let metrics = metrics_by_target.get(&t.id);
            let spark = spark_by_target
                .get(&t.id)
                .cloned()
                .unwrap_or_else(|| vec![None; SPARK_BUCKETS]);
            let dt = confirmed.then(|| downtime_by_target.get(&t.id).copied().unwrap_or(0));
            let mut row = DashboardRow::build(
                t.id,
                t.name,
                kind,
                address,
                t.enabled,
                metrics,
                spark,
                dt,
                window_secs,
            );
            // No live probe overrides the stale last status with grey "no data".
            if silenced.contains(&t.id) {
                row.last_status = "no_data";
            }
            tally_status(&mut status_counts, &row);
            if let Some(idx) = TYPE_CHIP_ORDER.iter().position(|k| *k == kind) {
                type_acc[idx] += 1;
            }
            row
        })
        .collect();

    let fleet_sparks = fleet_sparks(&spark_rows, spark_from);
    let checks_successful_label = format!("{} successful", format_count(checks_up));
    let current = PriorPeriodSummary {
        checks_total,
        checks_up,
        avg_ms: avg_ms_current,
    };
    // Time-weighted over sampled monitors so the fleet KPI matches the rows.
    let fleet_uptime_label = if confirmed {
        let mut total_down = 0i64;
        let mut sampled = 0i64;
        for (id, m) in &metrics_by_target {
            if m.samples > 0 {
                sampled += 1;
                total_down += downtime_by_target.get(id).copied().unwrap_or(0);
            }
        }
        if sampled > 0 {
            format!(
                "{:.2}%",
                uptime_pct_from_downtime(total_down, window_secs * sampled)
            )
        } else {
            pct_label(checks_total, checks_up)
        }
    } else {
        pct_label(checks_total, checks_up)
    };
    let kpis = DashboardKpis {
        uptime_pct_label: fleet_uptime_label,
        avg_response_ms_label: avg_response_label(avg_ms_current, checks_total),
        checks_label: format_count(checks_total),
        checks_successful_label: checks_successful_label.clone(),
        incidents,
    };
    let kpi_cards = build_kpi_cards(
        &kpis,
        range,
        checks_successful_label,
        &current,
        &prior,
        &fleet_sparks,
    );

    let now = Utc::now();
    let active_incidents: Vec<DashboardActiveIncident> = active_raw
        .into_iter()
        .map(|i| DashboardActiveIncident::build(i, now))
        .collect();

    let matches = rows.len();
    let ribbon = build_fleet_ribbon(&ribbon_rows, ribbon_from, &target_names);
    Ok(DashboardSnapshot {
        rows: Arc::from(rows.into_boxed_slice()),
        kpi_cards: Arc::from(kpi_cards.into_boxed_slice()),
        matches,
        truncated,
        active_incidents: Arc::from(active_incidents.into_boxed_slice()),
        status_counts,
        type_counts: Arc::from(build_type_counts(type_acc).into_boxed_slice()),
        ribbon,
    })
}

const ACTIVE_INCIDENTS_LIMIT: usize = 5;
const TYPE_CHIP_ORDER: &[&str] = &[
    "HTTP",
    "TCP",
    "PING",
    "HEARTBEAT",
    "DNS",
    "TLS",
    "DOMAIN",
    "FLOW",
];

fn tally_status(counts: &mut StatusCounts, row: &DashboardRow) {
    if !row.enabled {
        counts.paused += 1;
        return;
    }
    match row.last_status {
        "up" => counts.up += 1,
        "degraded" => counts.degraded += 1,
        "down" | "error" => counts.down += 1,
        _ => {}
    }
}

fn build_type_counts(acc: [u32; TYPE_CHIP_ORDER.len()]) -> Vec<TypeCount> {
    let total: u32 = acc.iter().sum();
    if total == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(1 + TYPE_CHIP_ORDER.len());
    out.push(TypeCount {
        label: "All",
        count: total,
        active: true,
    });
    for (idx, &label) in TYPE_CHIP_ORDER.iter().enumerate() {
        if acc[idx] > 0 {
            out.push(TypeCount {
                label,
                count: acc[idx],
                active: false,
            });
        }
    }
    out
}

/// Fleet metrics over the sparkline window — one series per KPI card, so each
/// card plots the metric it names.
#[derive(Default)]
struct FleetSparks {
    uptime: Vec<Option<f32>>,
    avg_ms: Vec<Option<f32>>,
    checks: Vec<Option<f32>>,
}

#[derive(Clone, Copy, Default)]
struct FleetSlot {
    latency_ms: f64,
    checks: u64,
    up: u64,
}

/// Every monitor's rollup, not just the rows the table shows — the KPI values
/// beside these lines are org-wide too.
fn fleet_sparks(rows: &[DashboardSparkBucket], from: DateTime<Utc>) -> FleetSparks {
    let from_ts = from.timestamp();
    let mut agg = [FleetSlot::default(); SPARK_BUCKETS];
    for r in rows {
        if !r.avg_ms.is_finite() {
            continue;
        }
        let slot = ((r.bucket_ts - from_ts) / 60).clamp(0, SPARK_BUCKETS as i64 - 1) as usize;
        // Weighted by check count: a monitor probed once in a minute must not
        // outvote one probed sixty times.
        agg[slot].latency_ms += f64::from(r.avg_ms) * r.checks as f64;
        agg[slot].checks += r.checks;
        agg[slot].up += r.up;
    }
    let series = |f: fn(&FleetSlot) -> f32| -> Vec<Option<f32>> {
        agg.iter().map(|s| (s.checks > 0).then(|| f(s))).collect()
    };
    FleetSparks {
        uptime: series(|s| (s.up as f64 / s.checks as f64 * 100.0) as f32),
        avg_ms: series(|s| (s.latency_ms / s.checks as f64) as f32),
        checks: series(|s| s.checks as f32),
    }
}

fn build_kpi_cards(
    kpis: &DashboardKpis,
    range: &'static str,
    checks_successful_label: String,
    current: &PriorPeriodSummary,
    prior: &PriorPeriodSummary,
    sparks: &FleetSparks,
) -> Vec<KpiCardSpec> {
    let incidents_html = format!(
        r#"Incidents (24h): <span class="{cls}">{n}</span>"#,
        cls = if kpis.incidents > 0 {
            "metric-alert"
        } else {
            "metric-quiet"
        },
        n = kpis.incidents,
    );
    let spark = |series: &[Option<f32>]| {
        let (path, fill, _) = render_spark_path(series);
        (path, fill)
    };
    // Percentages get the full 0..100 scale — autoscaled, a 99.99 → 100 wiggle
    // would draw the same cliff as an outage.
    let (uptime_path, uptime_fill, _) = render_spark_path_domain(&sparks.uptime, 0.0, 100.0);
    let (avg_path, avg_fill) = spark(&sparks.avg_ms);
    let (checks_path, checks_fill) = spark(&sparks.checks);
    vec![
        KpiCardSpec {
            label: format!("Uptime · {range}"),
            value: kpis.uptime_pct_label.clone(),
            hint_html: incidents_html,
            delta: uptime_delta(current, prior),
            spark_tint: "ok",
            spark_path: uptime_path,
            spark_fill: uptime_fill,
        },
        KpiCardSpec {
            label: format!("Avg response · {range}"),
            value: kpis.avg_response_ms_label.clone(),
            hint_html: "across all monitors".into(),
            delta: avg_delta(current, prior),
            spark_tint: "ink",
            spark_path: avg_path,
            spark_fill: avg_fill,
        },
        KpiCardSpec {
            label: format!("Checks · {range}"),
            value: kpis.checks_label.clone(),
            hint_html: checks_successful_label,
            delta: checks_delta(current, prior),
            spark_tint: "info",
            spark_path: checks_path,
            spark_fill: checks_fill,
        },
    ]
}

/// Polarity of a metric — whether a higher value is good, bad, or
/// just informational. Drives the `metric-delta--{up,down,flat}` class
/// (a green ↑ on uptime is the same class as a green ↓ on latency).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Polarity {
    /// Bigger is better — uptime.
    HigherIsBetter,
    /// Smaller is better — response time.
    LowerIsBetter,
    /// Direction is informational only — checks count.
    Neutral,
}

/// Standard chip for a non-zero Δ. `signed` carries the sign for the
/// arrow; polarity decides which colour class wins.
pub(crate) fn signed_delta(polarity: Polarity, signed: f64, body: String) -> KpiDelta {
    let arrow = if signed > 0.0 { "↑" } else { "↓" };
    let class = match polarity {
        Polarity::HigherIsBetter if signed > 0.0 => "metric-delta--up",
        Polarity::HigherIsBetter => "metric-delta--down",
        Polarity::LowerIsBetter if signed < 0.0 => "metric-delta--up",
        Polarity::LowerIsBetter => "metric-delta--down",
        Polarity::Neutral => "metric-delta--flat",
    };
    KpiDelta { class, arrow, body }
}

pub(crate) fn flat_delta() -> KpiDelta {
    KpiDelta {
        class: "metric-delta--flat",
        arrow: "±",
        body: "unchanged".into(),
    }
}

/// Δ chip for a percentage-point change; collapses to "unchanged" below
/// display precision so the chip never contradicts the shown value.
pub(crate) fn uptime_pp_delta(cur_pct: f64, prior_pct: f64) -> KpiDelta {
    let diff = ((cur_pct - prior_pct) * 100.0).round() / 100.0;
    if diff.abs() < 0.01 {
        return flat_delta();
    }
    signed_delta(Polarity::HigherIsBetter, diff, format!("{diff:+.2} pp"))
}

/// Δ chip for a raw count change (up / down / errors).
pub(crate) fn count_delta(cur: u64, prior: u64, polarity: Polarity) -> KpiDelta {
    let diff = cur as i64 - prior as i64;
    if diff == 0 {
        return flat_delta();
    }
    signed_delta(polarity, diff as f64, format!("{diff:+}"))
}

fn uptime_pct(s: &PriorPeriodSummary) -> Option<f64> {
    if s.checks_total == 0 {
        None
    } else {
        Some((s.checks_up as f64 / s.checks_total as f64) * 100.0)
    }
}

fn uptime_delta(cur: &PriorPeriodSummary, prior: &PriorPeriodSummary) -> Option<KpiDelta> {
    Some(uptime_pp_delta(uptime_pct(cur)?, uptime_pct(prior)?))
}

fn avg_delta(cur: &PriorPeriodSummary, prior: &PriorPeriodSummary) -> Option<KpiDelta> {
    if cur.checks_total == 0 || prior.checks_total == 0 {
        return None;
    }
    let diff = cur.avg_ms as i64 - prior.avg_ms as i64;
    if diff == 0 {
        return Some(flat_delta());
    }
    Some(signed_delta(
        Polarity::LowerIsBetter,
        diff as f64,
        format!("{diff:+} ms"),
    ))
}

fn checks_delta(cur: &PriorPeriodSummary, prior: &PriorPeriodSummary) -> Option<KpiDelta> {
    if cur.checks_total == 0 || prior.checks_total == 0 {
        return None;
    }
    let diff = cur.checks_total as i64 - prior.checks_total as i64;
    if diff == 0 {
        return Some(flat_delta());
    }
    let body = if diff >= 0 {
        format!("+{}", format_count(diff as u64))
    } else {
        format!("-{}", format_count(diff.unsigned_abs()))
    };
    Some(signed_delta(Polarity::Neutral, diff as f64, body))
}

/// Map CH ribbon rows → fixed-length 48-seg view. Buckets the storage
/// layer omitted (no samples) become `none`. Aggregate uptime label is
/// computed from the same sample totals so the displayed % matches the
/// segs the operator sees. `from` must already be bucket-aligned (see
/// `snap_to_bucket`) so labels line up with the CH `toStartOfInterval`
/// grid.
fn build_fleet_ribbon(
    rows: &[FleetRibbonBucket],
    from: DateTime<Utc>,
    names: &HashMap<Uuid, String>,
) -> FleetRibbon {
    let from_ts = from.timestamp();
    let bucket = RIBBON_BUCKET_SECONDS as i64;
    let mut filled: [(u64, u64); RIBBON_BUCKETS] = [(0, 0); RIBBON_BUCKETS];
    let mut down_by_slot: [Vec<Uuid>; RIBBON_BUCKETS] = std::array::from_fn(|_| Vec::new());
    let mut total_samples: u64 = 0;
    let mut total_up: u64 = 0;
    for r in rows {
        let offset = r.bucket_ts - from_ts;
        if offset < 0 {
            continue;
        }
        let slot = offset / bucket;
        if slot >= RIBBON_BUCKETS as i64 {
            continue;
        }
        let slot = slot as usize;
        filled[slot].0 += r.samples;
        filled[slot].1 += r.up;
        down_by_slot[slot].extend_from_slice(&r.down_targets);
        total_samples += r.samples;
        total_up += r.up;
    }
    let mut segs: Vec<FleetRibbonSeg> = Vec::with_capacity(RIBBON_BUCKETS);
    for (i, (samples, up)) in filled.iter().enumerate() {
        let slot_start = from + Duration::seconds(i as i64 * bucket);
        let down = std::mem::take(&mut down_by_slot[i]);
        let (class, stat) = if *samples == 0 {
            ("none", "no data".to_string())
        } else {
            let pct = (*up as f64 / *samples as f64) * 100.0;
            (ribbon_class(pct), format!("{pct:.1}%"))
        };
        let down_preview: Vec<String> = down
            .iter()
            .take(DOWN_PREVIEW)
            .map(|id| names.get(id).cloned().unwrap_or_else(|| "unknown".into()))
            .collect();
        segs.push(FleetRibbonSeg {
            class,
            time: slot_start.format("%H:%M").to_string(),
            stat,
            down_preview: Arc::from(down_preview.into_boxed_slice()),
            bucket_ts: from_ts + i as i64 * bucket,
            down_targets: Arc::from(down.into_boxed_slice()),
        });
    }
    FleetRibbon {
        segs: Arc::from(segs.into_boxed_slice()),
        uptime_label: pct_label(total_samples, total_up),
    }
}

/// Round `t` down to the nearest `bucket_seconds` boundary so the
/// returned `from` matches a CH `toStartOfInterval(_, INTERVAL bucket
/// SECOND)` grid line.
fn snap_to_bucket(t: DateTime<Utc>, bucket_seconds: u32) -> DateTime<Utc> {
    let b = bucket_seconds.max(1) as i64;
    let snapped = (t.timestamp().div_euclid(b)) * b;
    DateTime::<Utc>::from_timestamp(snapped, 0).unwrap_or(t)
}

pub(crate) fn ribbon_class(pct: f64) -> &'static str {
    if pct >= 99.9 {
        "op"
    } else if pct >= 95.0 {
        "deg"
    } else {
        "maj"
    }
}

fn range_span(key: &'static str) -> Duration {
    match key {
        "7d" => Duration::days(7),
        "30d" => Duration::days(30),
        "90d" => Duration::days(90),
        _ => Duration::hours(24),
    }
}

pub(crate) fn pct_label(total: u64, up: u64) -> String {
    match uptime_pct(&PriorPeriodSummary {
        checks_total: total,
        checks_up: up,
        avg_ms: 0,
    }) {
        Some(p) => format!("{p:.2}%"),
        None => "—".into(),
    }
}

fn avg_response_label(avg_ms: u32, samples: u64) -> String {
    if samples == 0 {
        return "—".into();
    }
    format!("{avg_ms} ms")
}

/// Compact count: "17.3k" / "1.2M" / "42". Matches the V3 KPI tile so
/// the strip stays single-line at any traffic level.
fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Bin sparkline rows into a fixed-length `Vec<Option<f32>>` per target,
/// oldest → newest. Missing minutes stay `None` so the SVG can break
/// the polyline (no fake "drop to zero" between gaps).
fn group_sparks(
    rows: &[DashboardSparkBucket],
    from: DateTime<Utc>,
) -> HashMap<Uuid, Vec<Option<f32>>> {
    let from_ts = from.timestamp();
    let mut out: HashMap<Uuid, Vec<Option<f32>>> = HashMap::new();
    for r in rows {
        let slot = ((r.bucket_ts - from_ts) / 60).clamp(0, SPARK_BUCKETS as i64 - 1) as usize;
        let buckets = out
            .entry(r.target_id)
            .or_insert_with(|| vec![None; SPARK_BUCKETS]);
        buckets[slot] = Some(r.avg_ms);
    }
    out
}

impl DashboardRow {
    #[allow(clippy::too_many_arguments)]
    fn build(
        id: Uuid,
        name: String,
        kind: &'static str,
        address: String,
        enabled: bool,
        metrics: Option<&DashboardMetrics>,
        spark: Vec<Option<f32>>,
        confirmed_downtime_secs: Option<i64>,
        window_secs: i64,
    ) -> Self {
        let (p50_label, p95_label, err_pct_label, uptime_pct_label, last_status, samples) =
            match metrics {
                Some(m) if m.samples > 0 => {
                    let err_pct = ((m.samples - m.up) as f64 / m.samples as f64) * 100.0;
                    let uptime_pct = match confirmed_downtime_secs {
                        Some(d) => uptime_pct_from_downtime(d, window_secs),
                        None => 100.0 - err_pct,
                    };
                    (
                        format!("{} ms", m.p50_ms),
                        format!("{} ms", m.p95_ms),
                        format!("{err_pct:.1}"),
                        format!("{uptime_pct:.2}"),
                        status_label(&m.last_status),
                        m.samples,
                    )
                }
                _ => ("—".into(), "—".into(), "—".into(), "—".into(), "", 0),
            };
        let (spark_path, spark_fill, spark_baseline_y) = render_spark_path(&spark);
        Self {
            id: id.to_string(),
            name,
            kind,
            address,
            enabled,
            last_status,
            p50_label,
            p95_label,
            err_pct_label,
            uptime_pct_label,
            samples,
            spark,
            spark_path,
            spark_fill,
            spark_baseline_y,
        }
    }
}

impl DashboardActiveIncident {
    fn build(raw: IncidentBrief, now: DateTime<Utc>) -> Self {
        let IncidentBrief {
            id,
            target_id,
            target_name,
            severity,
            started_at,
            public_title,
            latest_update,
            ..
        } = raw;
        let title = public_title
            .filter(|t| !t.trim().is_empty())
            .or_else(|| (!target_name.is_empty()).then_some(target_name))
            .unwrap_or_else(|| "Active incident".into());
        let age_secs = (now - started_at).num_seconds().max(0);
        Self {
            id: id.to_string(),
            target_id: target_id.to_string(),
            title,
            age_label: HumanDur(age_secs).to_string(),
            severity_label: severity_display(severity),
            severity_class: severity_class(severity),
            latest_update: latest_update.map(|u| DashboardIncidentUpdate {
                posted_at: u.posted_at,
                phase_label: phase_display(u.phase),
                message: u.message,
            }),
        }
    }
}

fn severity_display(s: IncidentSeverity) -> &'static str {
    match s {
        IncidentSeverity::Minor => "MINOR",
        IncidentSeverity::Major => "MAJOR",
        IncidentSeverity::Critical => "CRITICAL",
    }
}

fn severity_class(s: IncidentSeverity) -> &'static str {
    match s {
        IncidentSeverity::Minor => "minor",
        IncidentSeverity::Major => "major",
        IncidentSeverity::Critical => "critical",
    }
}

fn phase_display(p: crate::domain::IncidentStatusPhase) -> &'static str {
    use crate::domain::IncidentStatusPhase::*;
    match p {
        Investigating => "Investigating",
        Identified => "Identified",
        Monitoring => "Monitoring",
        Resolved => "Resolved",
        Postmortem => "Postmortem",
    }
}

fn status_label(raw: &str) -> &'static str {
    match raw {
        "up" => "up",
        "down" => "down",
        "degraded" => "degraded",
        "error" => "error",
        _ => "",
    }
}

/// Per-row sparkline into a `160×22` viewport. Returns `(line, fill, baseline_y)`.
///
/// Line connects across `None` gaps so a monitor sampled slower than the 1-min
/// bucket still renders. x spans the full window — data shorter than the window
/// fills the recent end instead of being stretched across it; y auto-scales to
/// the row's own min/max. A lone sample is a zero-length segment — a dot once
/// the path's round linecap (CSS) renders it. Fill is the line closed to the
/// baseline, empty below two samples. `baseline_y` carries the dashed no-data rule.
pub(crate) fn render_spark_path(spark: &[Option<f32>]) -> (String, String, u32) {
    // Treat NaN/Inf as missing — CH `avgMerge` returns NaN for empty
    // groups and a single non-finite poisons min/max, producing
    // `"M NaN NaN"` paths that browsers silently drop. The empty case (min/max
    // stay sentinels, unused) is handled inside the domain renderer.
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for v in spark.iter().filter_map(|o| o.filter(|v| v.is_finite())) {
        min = min.min(v);
        max = max.max(v);
    }
    render_spark_path_domain(spark, min, max)
}

/// Like [`render_spark_path`] but maps values onto a fixed `[min, max]`
/// domain instead of the series' own range — an availability line then sits
/// flat at the top when healthy and only notches down on real dips.
pub(crate) fn render_spark_path_domain(
    spark: &[Option<f32>],
    min: f32,
    max: f32,
) -> (String, String, u32) {
    const W: f32 = 160.0;
    const H: f32 = 22.0;
    let baseline_y = (H / 2.0).round() as u32;
    let finite = |o: &Option<f32>| o.filter(|v| v.is_finite());
    let present: Vec<f32> = spark.iter().filter_map(finite).collect();
    if present.is_empty() {
        return (String::new(), String::new(), baseline_y);
    }
    let span = (max - min).max(1.0);

    let step = W / (spark.len().saturating_sub(1).max(1)) as f32;

    let points: Vec<(f32, f32)> = spark
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| finite(slot).map(|v| (i, v)))
        .map(|(i, v)| {
            let x = step * i as f32;
            let y = (1.0 - (v - min) / span) * H;
            (x, y)
        })
        .collect();

    // Build the body once; fill derives from it so the two cannot drift apart.
    let mut line = String::with_capacity(points.len() * 14);
    let (fx, fy) = points[0];
    write!(line, "M{fx:.1} {fy:.1}").unwrap();
    for &(x, y) in &points[1..] {
        write!(line, " L{x:.1} {y:.1}").unwrap();
    }

    let fill = if points.len() >= 2 {
        let (lx, _) = *points.last().unwrap();
        let mut fill = String::with_capacity(line.len() + 32);
        fill.push_str(&line);
        write!(fill, " L{lx:.1} {H:.1} L{fx:.1} {H:.1} Z").unwrap();
        fill
    } else {
        // zero-length segment → dot under the round linecap
        write!(line, " L{fx:.1} {fy:.1}").unwrap();
        String::new()
    };

    (line, fill, baseline_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_kpis() -> DashboardKpis {
        DashboardKpis {
            uptime_pct_label: "99.92%".into(),
            avg_response_ms_label: "142 ms".into(),
            checks_label: "17.3k".into(),
            checks_successful_label: "17.0k successful".into(),
            incidents: 3,
        }
    }

    fn sample_kpi_cards() -> Vec<KpiCardSpec> {
        let zero = PriorPeriodSummary::default();
        build_kpi_cards(
            &sample_kpis(),
            "24h",
            "17.0k successful".into(),
            &zero,
            &zero,
            &FleetSparks::default(),
        )
    }

    fn sample_row(name: &str, status: &'static str) -> DashboardRow {
        let spark = vec![Some(100.0), Some(120.0), None, Some(110.0)];
        let (spark_path, spark_fill, baseline_y) = render_spark_path(&spark);
        DashboardRow {
            id: "11111111-1111-1111-1111-111111111111".into(),
            name: name.into(),
            kind: "HTTP",
            address: "https://api.example.com".into(),
            enabled: true,
            last_status: status,
            p50_label: "100 ms".into(),
            p95_label: "120 ms".into(),
            err_pct_label: "0.0".into(),
            uptime_pct_label: "99.92".into(),
            samples: 720,
            spark,
            spark_path,
            spark_fill,
            spark_baseline_y: baseline_y,
        }
    }

    fn sample_ribbon() -> FleetRibbon {
        build_fleet_ribbon(&[], snapped_from(), &HashMap::new())
    }

    fn sample_page() -> DashboardPage {
        let rows = vec![sample_row("api", "up"), sample_row("worker", "degraded")];
        DashboardPage {
            active_tab: "dashboard",
            range: "24h",
            range_options: build_range_options("24h", &RANGE_KEYS),
            kpi_cards: Arc::from(sample_kpi_cards().into_boxed_slice()),
            rows: Arc::from(rows.into_boxed_slice()),
            matches: 2,
            truncated: false,
            onboarding: false,
            active_incidents: Arc::from(Vec::<DashboardActiveIncident>::new().into_boxed_slice()),
            status_counts: StatusCounts {
                up: 1,
                degraded: 1,
                ..Default::default()
            },
            type_counts: Arc::from(
                vec![TypeCount {
                    label: "All",
                    count: 2,
                    active: true,
                }]
                .into_boxed_slice(),
            ),
            ribbon: sample_ribbon(),
            regions: Vec::new(),
            selected_region: None,
            status_options: build_range_options(FILTER_ANY, &STATUS_FILTERS),
            selected_status: None,
            selected_kind: None,
            drill: None,
            restored_notice: false,
            joined_notice: None,
            invite_missed_notice: false,
        }
    }

    #[test]
    fn page_renders_chrome_and_kpis() {
        let html = sample_page().render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Dashboard"));
        assert!(html.contains("99.92%"));
        assert!(html.contains("142 ms"));
        assert!(html.contains("17.3k"));
        // Range tabs.
        for k in &RANGE_KEYS {
            assert!(html.contains(&format!(">{k}<")));
        }
        // Row cells.
        assert!(html.contains("api"));
        assert!(html.contains("worker"));
        // htmx swap target for tab clicks.
        assert!(html.contains(r#"id="dashboard-table""#));
        assert!(html.contains(r#"hx-get="/web/partials/dashboard"#));
    }

    #[test]
    fn partial_omits_chrome() {
        let partial = DashboardTablePartial {
            range: "7d",
            range_options: build_range_options("7d", &RANGE_KEYS),
            kpi_cards: Arc::from(sample_kpi_cards().into_boxed_slice()),
            rows: Arc::from(vec![sample_row("api", "up")].into_boxed_slice()),
            matches: 1,
            truncated: false,
            active_incidents: Arc::from(Vec::<DashboardActiveIncident>::new().into_boxed_slice()),
            status_counts: StatusCounts {
                up: 1,
                ..Default::default()
            },
            type_counts: Arc::from(
                vec![TypeCount {
                    label: "All",
                    count: 1,
                    active: true,
                }]
                .into_boxed_slice(),
            ),
            ribbon: sample_ribbon(),
            regions: Vec::new(),
            selected_region: None,
            status_options: build_range_options(FILTER_ANY, &STATUS_FILTERS),
            selected_status: None,
            selected_kind: None,
            drill: None,
        };
        let html = partial.render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(!html.contains("<nav"));
        assert!(html.contains(r#"id="dashboard-table""#));
        assert!(html.contains("99.92%"));
    }

    #[test]
    fn onboarding_state_skips_table() {
        let kpis = DashboardKpis {
            uptime_pct_label: "—".into(),
            avg_response_ms_label: "—".into(),
            checks_label: "0".into(),
            checks_successful_label: "0 successful".into(),
            incidents: 0,
        };
        let page = DashboardPage {
            active_tab: "dashboard",
            range: "24h",
            range_options: build_range_options("24h", &RANGE_KEYS),
            kpi_cards: Arc::from({
                let zero = PriorPeriodSummary::default();
                build_kpi_cards(
                    &kpis,
                    "24h",
                    "0 successful".into(),
                    &zero,
                    &zero,
                    &FleetSparks::default(),
                )
                .into_boxed_slice()
            }),
            rows: Arc::from(Vec::<DashboardRow>::new().into_boxed_slice()),
            matches: 0,
            truncated: false,
            onboarding: true,
            active_incidents: Arc::from(Vec::<DashboardActiveIncident>::new().into_boxed_slice()),
            status_counts: StatusCounts::default(),
            type_counts: Arc::from(Vec::<TypeCount>::new().into_boxed_slice()),
            ribbon: sample_ribbon(),
            regions: Vec::new(),
            selected_region: None,
            status_options: build_range_options(FILTER_ANY, &STATUS_FILTERS),
            selected_status: None,
            selected_kind: None,
            drill: None,
            restored_notice: false,
            joined_notice: None,
            invite_missed_notice: false,
        };
        let html = page.render().unwrap();
        assert!(html.contains("nothing to watch yet."));
        assert!(html.contains("add your first monitor"));
        assert!(!html.contains(r#"id="dashboard-table""#));
    }

    #[test]
    fn format_count_compacts() {
        assert_eq!(format_count(42), "42");
        assert_eq!(format_count(1_500), "1.5k");
        assert_eq!(format_count(17_300), "17.3k");
        assert_eq!(format_count(2_400_000), "2.4M");
    }

    #[test]
    fn pct_label_handles_empty_window() {
        assert_eq!(pct_label(0, 0), "—");
        assert_eq!(pct_label(1_000, 999), "99.90%");
    }

    fn spark_bucket(
        from: DateTime<Utc>,
        minute: i64,
        avg_ms: f32,
        checks: u64,
        up: u64,
    ) -> DashboardSparkBucket {
        DashboardSparkBucket {
            target_id: Uuid::nil(),
            bucket_ts: from.timestamp() + minute * 60,
            avg_ms,
            checks,
            up,
        }
    }

    #[test]
    fn fleet_sparks_weight_by_check_count() {
        let from = Utc::now() - Duration::minutes(SPARK_MINUTES);
        let sparks = fleet_sparks(
            &[
                spark_bucket(from, 0, 100.0, 1, 1),
                spark_bucket(from, 0, 200.0, 59, 30),
            ],
            from,
        );
        // A mean of the two means would say 150 ms and 75 % uptime.
        assert_eq!(sparks.avg_ms[0].unwrap().round(), 198.0);
        assert_eq!(sparks.uptime[0].unwrap().round(), 52.0);
        assert_eq!(sparks.checks[0].unwrap(), 60.0);
        assert!(sparks.avg_ms[1].is_none());
    }

    #[test]
    fn each_kpi_card_plots_its_own_metric() {
        let from = Utc::now() - Duration::minutes(SPARK_MINUTES);
        // Latency climbs while checks and uptime fall — three shapes, not one.
        let sparks = fleet_sparks(
            &[
                spark_bucket(from, 0, 100.0, 40, 40),
                spark_bucket(from, 1, 400.0, 10, 5),
            ],
            from,
        );
        let zero = PriorPeriodSummary::default();
        let cards = build_kpi_cards(
            &sample_kpis(),
            "24h",
            "17.0k successful".into(),
            &zero,
            &zero,
            &sparks,
        );
        let paths: Vec<&str> = cards.iter().map(|c| c.spark_path.as_str()).collect();
        assert!(paths.iter().all(|p| !p.is_empty()));
        assert_ne!(paths[0], paths[1]);
        assert_ne!(paths[1], paths[2]);
        assert_ne!(paths[0], paths[2]);
    }

    #[test]
    fn render_spark_path_connects_across_gaps() {
        let (line, fill, _) = render_spark_path(&[Some(1.0), Some(2.0), None, Some(3.0)]);
        assert_eq!(line.matches('M').count(), 1);
        assert_eq!(line.matches('L').count(), 2);
        assert!(!fill.is_empty());
    }

    #[test]
    fn render_spark_path_renders_sparse_interval_monitor() {
        // ≥2-min cadence leaves every filled bucket isolated.
        let mut spark = vec![None; 60];
        for (i, slot) in spark.iter_mut().enumerate() {
            if i % 5 == 0 {
                *slot = Some(100.0 + i as f32);
            }
        }
        let (line, fill, _) = render_spark_path(&spark);
        assert!(line.starts_with('M'));
        assert_eq!(line.matches('M').count(), 1);
        assert!(line.contains('L'));
        assert!(!fill.is_empty());
    }

    #[test]
    fn render_spark_path_partial_window_fills_recent_end_only() {
        // Half-window of data must start past the midpoint, not from x=0.
        let mut spark = vec![None; 60];
        for slot in spark.iter_mut().skip(30) {
            *slot = Some(50.0);
        }
        let (line, _, _) = render_spark_path(&spark);
        let first_x: f32 = line
            .trim_start_matches('M')
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let last_x: f32 = line
            .rsplit('L')
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(first_x > 80.0, "first sample x={first_x}, want recent half");
        assert!(
            (last_x - 160.0).abs() < 0.5,
            "newest sample x={last_x}, want right edge"
        );
    }

    #[test]
    fn render_spark_path_drops_non_finite_samples() {
        // CH avgMerge returns NaN for empty groups; one non-finite must not
        // poison min/max nor emit an "M NaN" path browsers silently drop.
        let (line, fill, _) =
            render_spark_path(&[Some(1.0), Some(f32::NAN), Some(f32::INFINITY), Some(3.0)]);
        assert!(!line.contains("NaN") && !line.contains("inf"));
        assert_eq!(line.matches('L').count(), 1);
        assert!(!fill.is_empty());
    }

    #[test]
    fn render_spark_path_lone_sample_draws_dot() {
        let mut spark = vec![None; 60];
        spark[30] = Some(42.0);
        let (line, fill, _) = render_spark_path(&spark);
        assert_eq!(line.matches('M').count(), 1);
        assert_eq!(line.matches('L').count(), 1);
        assert!(fill.is_empty());
    }

    #[test]
    fn render_spark_path_empty_when_no_data() {
        assert_eq!(render_spark_path(&vec![None; 60]).0, "");
    }

    #[test]
    fn render_spark_path_domain_fixed_scale_independent_of_series_range() {
        let data = [Some(100.0_f32), Some(100.0), Some(50.0)];
        let (fixed, _, _) = render_spark_path_domain(&data, 0.0, 100.0);
        let (auto, _, _) = render_spark_path(&data);
        // Healthy 100 sits at the top (y=0) on both.
        assert!(fixed.starts_with("M0.0 0.0"), "fixed: {fixed}");
        // The dip to 50 lands mid-height on the fixed 0–100 scale, but at the
        // bottom when the line normalises to its own [50,100] range.
        assert!(fixed.trim_end().ends_with("11.0"), "fixed dip: {fixed}");
        assert!(auto.trim_end().ends_with("22.0"), "auto dip: {auto}");
    }

    fn row_with(enabled: bool, status: &'static str) -> DashboardRow {
        DashboardRow {
            id: String::new(),
            name: String::new(),
            kind: "HTTP",
            address: String::new(),
            enabled,
            last_status: status,
            p50_label: String::new(),
            p95_label: String::new(),
            err_pct_label: String::new(),
            uptime_pct_label: String::new(),
            samples: 0,
            spark: Vec::new(),
            spark_path: String::new(),
            spark_fill: String::new(),
            spark_baseline_y: 0,
        }
    }

    fn sample_snapshot(rows: Vec<DashboardRow>) -> DashboardSnapshot {
        let n = rows.len();
        DashboardSnapshot {
            rows: Arc::from(rows.into_boxed_slice()),
            kpi_cards: Arc::from(sample_kpi_cards().into_boxed_slice()),
            matches: n,
            truncated: false,
            active_incidents: Arc::from(Vec::<DashboardActiveIncident>::new().into_boxed_slice()),
            status_counts: StatusCounts::default(),
            type_counts: Arc::from(
                vec![
                    TypeCount {
                        label: "All",
                        count: n as u32,
                        active: true,
                    },
                    TypeCount {
                        label: "HTTP",
                        count: n as u32,
                        active: false,
                    },
                ]
                .into_boxed_slice(),
            ),
            ribbon: sample_ribbon(),
        }
    }

    #[test]
    fn type_filter_matches_kind_case_insensitively() {
        let snapshot = sample_snapshot(vec![sample_row("api", "up"), sample_row("worker", "up")]);
        let (rows, matches) = filter_rows(&snapshot, None, Some("http"), None);
        assert_eq!(matches, 2);
        assert_eq!(rows.len(), 2);
        let (_, matches) = filter_rows(&snapshot, None, Some("tcp"), None);
        assert_eq!(matches, 0);

        let chips = type_chips(&snapshot, None, Some("http"));
        assert!(!chips[0].active, "All must drop active under a type filter");
        assert!(chips[1].active);
        let chips = type_chips(&snapshot, None, None);
        assert!(chips[0].active);
        assert!(!chips[1].active);
    }

    #[test]
    fn type_chip_counts_follow_status_filter() {
        let snapshot = sample_snapshot(vec![sample_row("api", "up"), sample_row("worker", "down")]);
        let chips = type_chips(&snapshot, Some("down"), None);
        assert_eq!(chips[0].count, 1, "All narrows to status matches");
        assert_eq!(chips[1].count, 1);
        let chips = type_chips(&snapshot, Some("paused"), None);
        assert_eq!(chips[0].count, 0, "chip stays visible at 0, not dropped");
        let chips = type_chips(&snapshot, None, None);
        assert_eq!(chips[0].count, 2, "no status filter keeps snapshot counts");
    }

    #[test]
    fn type_filters_mirror_chip_order() {
        assert_eq!(TYPE_FILTERS[0], FILTER_ANY);
        assert_eq!(TYPE_FILTERS.len(), TYPE_CHIP_ORDER.len() + 1);
        for (key, label) in TYPE_FILTERS[1..].iter().zip(TYPE_CHIP_ORDER) {
            assert_eq!(*key, label.to_ascii_lowercase());
        }
        // One chip per check kind — a new CheckSpec variant must add its chip.
        assert_eq!(
            TYPE_CHIP_ORDER.len(),
            crate::domain::CheckSpec::ALL_KINDS.len()
        );
    }

    #[test]
    fn status_filter_paused_matches_disabled_only() {
        assert!(row_matches_status(&row_with(false, "up"), "paused"));
        assert!(!row_matches_status(&row_with(true, "up"), "paused"));
        assert!(!row_matches_status(&row_with(false, "down"), "down"));
    }

    #[test]
    fn status_filter_down_includes_error() {
        assert!(row_matches_status(&row_with(true, "down"), "down"));
        assert!(row_matches_status(&row_with(true, "error"), "down"));
        assert!(!row_matches_status(&row_with(true, "up"), "down"));
    }

    #[test]
    fn status_filter_up_excludes_never_reported() {
        assert!(row_matches_status(&row_with(true, "up"), "up"));
        assert!(!row_matches_status(&row_with(true, ""), "up"));
    }

    #[test]
    fn tally_status_disabled_wins_over_status() {
        let mut c = StatusCounts::default();
        tally_status(&mut c, &row_with(false, "down"));
        assert_eq!(c.paused, 1);
        assert_eq!(c.down, 0);
    }

    #[test]
    fn tally_status_error_counts_as_down() {
        let mut c = StatusCounts::default();
        tally_status(&mut c, &row_with(true, "error"));
        assert_eq!(c.down, 1);
    }

    #[test]
    fn tally_status_ignores_unknown_status() {
        let mut c = StatusCounts::default();
        tally_status(&mut c, &row_with(true, ""));
        assert_eq!(c.up, 0);
        assert_eq!(c.down, 0);
    }

    #[test]
    fn build_type_counts_empty_returns_empty() {
        assert!(build_type_counts([0; TYPE_CHIP_ORDER.len()]).is_empty());
    }

    #[test]
    fn build_type_counts_emits_all_plus_nonzero_kinds() {
        let idx = |label| TYPE_CHIP_ORDER.iter().position(|k| *k == label).unwrap();
        let mut acc = [0; TYPE_CHIP_ORDER.len()];
        acc[idx("HTTP")] = 3;
        acc[idx("DNS")] = 1;
        let counts = build_type_counts(acc);
        assert_eq!(counts.len(), 3);
        assert_eq!(counts[0].label, "All");
        assert_eq!(counts[0].count, 4);
        assert_eq!(counts[1].label, "HTTP");
        assert_eq!(counts[2].label, "DNS");
    }

    fn prior(total: u64, up: u64, avg_ms: u32) -> PriorPeriodSummary {
        PriorPeriodSummary {
            checks_total: total,
            checks_up: up,
            avg_ms,
        }
    }

    #[test]
    fn uptime_delta_hides_when_either_side_has_no_data() {
        assert!(uptime_delta(&prior(100, 100, 0), &prior(0, 0, 0)).is_none());
        assert!(uptime_delta(&prior(0, 0, 0), &prior(100, 100, 0)).is_none());
    }

    #[test]
    fn uptime_delta_higher_is_up_class() {
        // 99.5% vs 98.0% → +1.50 pp better → metric-delta--up.
        let d = uptime_delta(&prior(1000, 995, 0), &prior(1000, 980, 0)).expect("delta");
        assert_eq!(d.class, "metric-delta--up");
        assert_eq!(d.arrow, "↑");
        assert_eq!(d.body, "+1.50 pp");
    }

    #[test]
    fn uptime_delta_lower_is_down_class() {
        let d = uptime_delta(&prior(1000, 950, 0), &prior(1000, 999, 0)).expect("delta");
        assert_eq!(d.class, "metric-delta--down");
        assert_eq!(d.arrow, "↓");
        assert_eq!(d.body, "-4.90 pp");
    }

    #[test]
    fn uptime_delta_flat_within_tolerance() {
        // 99.9994% vs 99.9990% → diff ≈ 0.0004 pp → quantizes to 0 → flat.
        let d = uptime_delta(&prior(1_000_000, 999_994, 0), &prior(1_000_000, 999_990, 0))
            .expect("delta");
        assert_eq!(d.class, "metric-delta--flat");
        assert_eq!(d.arrow, "±");
        assert_eq!(d.body, "unchanged");
    }

    #[test]
    fn uptime_delta_quantize_threshold_matches_display() {
        // 0.005 pp rounds *up* to 0.01 → should render the non-flat chip
        // and the displayed value must equal the rounded threshold.
        let d =
            uptime_delta(&prior(100_000, 99_995, 0), &prior(100_000, 99_990, 0)).expect("delta");
        assert_eq!(d.class, "metric-delta--up");
        assert_eq!(d.body, "+0.01 pp");
    }

    #[test]
    fn avg_delta_faster_is_up_class() {
        // Avg dropped (better) — green.
        let d = avg_delta(&prior(100, 100, 120), &prior(100, 100, 180)).expect("delta");
        assert_eq!(d.class, "metric-delta--up");
        assert_eq!(d.body, "-60 ms");
    }

    #[test]
    fn avg_delta_slower_is_down_class() {
        let d = avg_delta(&prior(100, 100, 200), &prior(100, 100, 150)).expect("delta");
        assert_eq!(d.class, "metric-delta--down");
        assert_eq!(d.body, "+50 ms");
    }

    #[test]
    fn checks_delta_neutral_direction() {
        let d = checks_delta(&prior(2_500, 2_500, 0), &prior(2_000, 2_000, 0)).expect("delta");
        assert_eq!(d.class, "metric-delta--flat");
        assert_eq!(d.arrow, "↑");
        assert!(d.body.contains("+500"), "{}", d.body);
    }

    #[test]
    fn checks_delta_hides_when_either_side_has_no_data() {
        assert!(checks_delta(&prior(100, 100, 0), &prior(0, 0, 0)).is_none());
        assert!(checks_delta(&prior(0, 0, 0), &prior(100, 100, 0)).is_none());
    }

    #[test]
    fn ribbon_class_partitions_by_uptime() {
        assert_eq!(ribbon_class(100.0), "op");
        assert_eq!(ribbon_class(99.9), "op");
        assert_eq!(ribbon_class(99.89), "deg");
        assert_eq!(ribbon_class(95.0), "deg");
        assert_eq!(ribbon_class(94.99), "maj");
        assert_eq!(ribbon_class(0.0), "maj");
    }

    fn snapped_from() -> DateTime<Utc> {
        snap_to_bucket(
            Utc::now() - Duration::hours(RIBBON_HOURS),
            RIBBON_BUCKET_SECONDS,
        )
    }

    #[test]
    fn build_fleet_ribbon_emits_48_segs_when_empty() {
        let from = snapped_from();
        let r = build_fleet_ribbon(&[], from, &HashMap::new());
        assert_eq!(r.segs.len(), RIBBON_BUCKETS);
        assert!(r.segs.iter().all(|s| s.class == "none"));
        assert_eq!(r.uptime_label, "—");
    }

    #[test]
    fn build_fleet_ribbon_classifies_rows_into_slots() {
        let from = snapped_from();
        let from_ts = from.timestamp();
        let bucket = RIBBON_BUCKET_SECONDS as i64;
        let rows = vec![
            FleetRibbonBucket {
                bucket_ts: from_ts,
                samples: 100,
                up: 100,
                down_targets: vec![],
            },
            FleetRibbonBucket {
                bucket_ts: from_ts + bucket,
                samples: 100,
                up: 97,
                down_targets: vec![],
            },
            FleetRibbonBucket {
                bucket_ts: from_ts + bucket * 2,
                samples: 100,
                up: 50,
                down_targets: vec![],
            },
        ];
        let r = build_fleet_ribbon(&rows, from, &HashMap::new());
        assert_eq!(r.segs[0].class, "op");
        assert_eq!(r.segs[1].class, "deg");
        assert_eq!(r.segs[2].class, "maj");
        assert_eq!(r.segs[3].class, "none");
        assert_eq!(r.segs[0].bucket_ts, from_ts);
        assert_eq!(r.segs[1].bucket_ts, from_ts + bucket);
        assert!(r.uptime_label.starts_with("82.")); // 247/300
    }

    #[test]
    fn build_fleet_ribbon_drops_out_of_window_rows() {
        let from = snapped_from();
        let from_ts = from.timestamp();
        // Storage `WHERE minute >= from AND minute < to` should already
        // filter these, but the view layer drops them defensively so a
        // clock skew can't smear data into edge slots.
        let rows = vec![
            FleetRibbonBucket {
                bucket_ts: from_ts - 10_000,
                samples: 10,
                up: 10,
                down_targets: vec![],
            },
            FleetRibbonBucket {
                bucket_ts: from_ts + (RIBBON_HOURS * 3600),
                samples: 10,
                up: 0,
                down_targets: vec![],
            },
            FleetRibbonBucket {
                bucket_ts: from_ts + (RIBBON_HOURS * 3600) + 10_000,
                samples: 10,
                up: 0,
                down_targets: vec![],
            },
        ];
        let r = build_fleet_ribbon(&rows, from, &HashMap::new());
        assert!(r.segs.iter().all(|s| s.class == "none"));
        assert_eq!(r.uptime_label, "—");
    }

    #[test]
    fn build_fleet_ribbon_handles_all_down_slot() {
        let from = snapped_from();
        let rows = vec![FleetRibbonBucket {
            bucket_ts: from.timestamp(),
            samples: 50,
            up: 0,
            down_targets: vec![],
        }];
        let r = build_fleet_ribbon(&rows, from, &HashMap::new());
        assert_eq!(r.segs[0].class, "maj");
        assert_eq!(r.segs[0].stat, "0.0%");
        assert_eq!(r.uptime_label, "0.00%");
    }

    #[test]
    fn build_fleet_ribbon_sums_multiple_rows_in_same_slot() {
        // Storage emits one row per CH bucket so this shouldn't happen,
        // but the +=-into-fixed-array contract is the whole point of the
        // stack array — pin it.
        let from = snapped_from();
        let rows = vec![
            FleetRibbonBucket {
                bucket_ts: from.timestamp(),
                samples: 40,
                up: 40,
                down_targets: vec![],
            },
            FleetRibbonBucket {
                bucket_ts: from.timestamp(),
                samples: 60,
                up: 56,
                down_targets: vec![],
            },
        ];
        let r = build_fleet_ribbon(&rows, from, &HashMap::new());
        assert_eq!(r.segs[0].class, "deg"); // 96/100 → 96 % → deg
        assert_eq!(r.uptime_label, "96.00%");
    }

    #[test]
    fn build_fleet_ribbon_previews_capped_down_names() {
        let from = snapped_from();
        let ids: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
        let names: HashMap<Uuid, String> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, format!("svc{i}")))
            .collect();
        let rows = vec![FleetRibbonBucket {
            bucket_ts: from.timestamp(),
            samples: 100,
            up: 60,
            down_targets: ids.clone(),
        }];
        let r = build_fleet_ribbon(&rows, from, &names);
        let seg = &r.segs[0];
        assert_eq!(
            &*seg.down_targets,
            ids.as_slice(),
            "drill keeps the full set"
        );
        assert_eq!(seg.down_targets.len(), 8);
        assert_eq!(seg.down_preview.len(), DOWN_PREVIEW, "preview capped");
        assert_eq!(&seg.down_preview[0], "svc0");
        assert_eq!(&seg.down_preview[5], "svc5");
    }

    fn ribbon_one_down() -> FleetRibbon {
        let from = snapped_from();
        let id = Uuid::new_v4();
        let mut names = HashMap::new();
        names.insert(id, "api".to_string());
        let rows = vec![FleetRibbonBucket {
            bucket_ts: from.timestamp(),
            samples: 10,
            up: 4,
            down_targets: vec![id],
        }];
        build_fleet_ribbon(&rows, from, &names)
    }

    #[test]
    fn ribbon_drill_cell_links_to_filter_when_inactive() {
        let ribbon = ribbon_one_down();
        let bucket = ribbon.segs[0].bucket_ts;
        let mut page = sample_page();
        page.ribbon = ribbon;
        page.drill = None;
        let html = page.render().unwrap();
        assert!(
            html.contains(&format!("down_at={bucket}")),
            "links to set the filter"
        );
        assert!(!html.contains("dashboard-ribbon__seg--active"));
    }

    #[test]
    fn ribbon_drill_cell_toggles_off_when_active() {
        let ribbon = ribbon_one_down();
        let bucket = ribbon.segs[0].bucket_ts;
        let mut page = sample_page();
        page.ribbon = ribbon;
        page.drill = Some(DrillChip {
            down_at: bucket,
            label: "00:00".into(),
        });
        let html = page.render().unwrap();
        assert!(
            html.contains("dashboard-ribbon__seg--active"),
            "active cell is ringed"
        );
        // The cell's own link drops down_at (toggle off)...
        let active_btn = html
            .split("dashboard-ribbon__seg--active")
            .nth(1)
            .unwrap()
            .split("</button>")
            .next()
            .unwrap();
        assert!(
            !active_btn.contains("down_at"),
            "active cell links to clear, not re-set"
        );
        // ...while the poll URL keeps it so the filter survives the 5s refresh.
        assert!(
            html.contains(&format!(
                "/web/partials/dashboard?range=24h&down_at={bucket}"
            )),
            "poll persists the active filter"
        );
    }

    #[test]
    fn snap_to_bucket_floors_to_grid() {
        let bucket = RIBBON_BUCKET_SECONDS as i64;
        let t = DateTime::<Utc>::from_timestamp(bucket * 100 + 137, 0).unwrap();
        let s = snap_to_bucket(t, RIBBON_BUCKET_SECONDS);
        assert_eq!(s.timestamp() % bucket, 0);
        assert_eq!(s.timestamp(), bucket * 100);
    }

    #[test]
    fn dashboard_active_incident_falls_back_to_target_name_then_default() {
        let now = Utc::now();
        let make = |public_title, target_name: &str| IncidentBrief {
            id: Uuid::nil(),
            target_id: Uuid::nil(),
            target_name: target_name.into(),
            severity: IncidentSeverity::Major,
            started_at: now - Duration::minutes(5),
            ended_at: None,
            public_title,
            latest_update: None,
        };
        assert_eq!(
            DashboardActiveIncident::build(make(Some("Outage".into()), "api"), now).title,
            "Outage"
        );
        assert_eq!(
            DashboardActiveIncident::build(make(None, "api"), now).title,
            "api"
        );
        assert_eq!(
            DashboardActiveIncident::build(make(Some("  ".into()), ""), now).title,
            "Active incident"
        );
    }
}
