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
use uuid::Uuid;

use crate::api::types::{DashboardMetrics, DashboardSparkBucket};
use crate::app::AppState;
use crate::domain::OrgId;
use crate::storage::{TargetFilter, TimeRange};
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::host::is_subdomain_public_request;
use crate::web::views::public_status::{self, StatusParams};
use crate::web::views::{RangeOption, build_range_options, describe_check, resolve_range_key};
use crate::web::{AuthedBrowser, CurrentOrg};

pub(crate) const RANGE_KEYS: [&str; 4] = ["24h", "7d", "30d", "90d"];
pub(crate) const DEFAULT_RANGE: &str = "24h";
/// Fixed sparkline window — "right-now" trend, decoupled from the
/// selected rollup range so an operator browsing 90 d still sees a
/// fresh 1 h trace per monitor.
const SPARK_MINUTES: i64 = 60;
const SPARK_BUCKETS: usize = SPARK_MINUTES as usize;
/// Cap rows on a single page render. Beyond this an org would benefit
/// from search/filter on `/targets`; rendering 5 k rows inline is a
/// browser-side hazard, not just a server cost.
const ROW_LIMIT: usize = 500;

#[derive(Debug, Default, Deserialize)]
pub struct DashboardParams {
    #[serde(default)]
    pub range: Option<String>,
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
    /// Empty buckets carry `None` so the renderer can break the line
    /// rather than drawing a misleading drop-to-zero spike.
    pub spark: Vec<Option<f32>>,
    /// Pre-rendered SVG path `d` for the sparkline polyline. Built
    /// once server-side so the template stays static markup and the
    /// browser does no per-row math on render.
    pub spark_path: String,
    /// SVG `<path d=…>` for the soft area fill under the line. One
    /// closed sub-path per contiguous run of ≥2 samples so gaps don't
    /// bridge across missing minutes.
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
    pub kpis: DashboardKpis,
    pub matches: usize,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct DashboardKpis {
    pub uptime_pct_label: String,
    pub avg_response_ms_label: String,
    pub checks_label: String,
    pub incidents: u64,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub active_tab: &'static str,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub kpis: DashboardKpis,
    pub rows: Arc<[DashboardRow]>,
    pub matches: usize,
    pub truncated: bool,
    pub onboarding: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/table.html")]
pub struct DashboardTablePartial {
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub kpis: DashboardKpis,
    pub rows: Arc<[DashboardRow]>,
    pub matches: usize,
    pub truncated: bool,
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
    let params = match Query::<DashboardParams>::try_from_uri(&parts.uri) {
        Ok(q) => q,
        Err(rej) => return rej.into_response(),
    };
    match index(auth, state, org, params).await {
        Ok(page) => page.into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn index(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    org: CurrentOrg,
    Query(params): Query<DashboardParams>,
) -> WebResult<DashboardPage> {
    let range = resolve_range_key(params.range.as_deref(), &RANGE_KEYS, DEFAULT_RANGE);
    let snapshot = load_snapshot(&state, org.0, range).await?;
    let onboarding = snapshot.matches == 0;
    Ok(DashboardPage {
        active_tab: "dashboard",
        range,
        range_options: build_range_options(range, &RANGE_KEYS),
        kpis: snapshot.kpis.clone(),
        rows: Arc::clone(&snapshot.rows),
        matches: snapshot.matches,
        truncated: snapshot.truncated,
        onboarding,
    })
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
    let snapshot = load_snapshot(&state, org.0, range).await?;
    let partial = DashboardTablePartial {
        range,
        range_options: build_range_options(range, &RANGE_KEYS),
        kpis: snapshot.kpis.clone(),
        rows: Arc::clone(&snapshot.rows),
        matches: snapshot.matches,
        truncated: snapshot.truncated,
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
    let snap = Arc::new(build_snapshot(state, org, range).await?);
    state
        .dashboard_page_cache
        .insert((org, range), Arc::clone(&snap));
    Ok(snap)
}

async fn build_snapshot(
    state: &AppState,
    org: OrgId,
    range: &'static str,
) -> WebResult<DashboardSnapshot> {
    let to = Utc::now();
    let from = to - range_span(range);
    let time_range = TimeRange { from, to };
    let spark_from = to - Duration::minutes(SPARK_MINUTES);

    let target_filter = TargetFilter {
        limit: Some(ROW_LIMIT + 1),
        offset: 0,
        tag: None,
        enabled: None,
    };

    let (mut targets, rollup, spark_rows, (checks_total, checks_up, incidents)) = tokio::try_join!(
        state.target_store.list(org, target_filter),
        state.results_store.dashboard_rollup(org, time_range),
        state.results_store.dashboard_sparkline(org, spark_from, to),
        state.results_store.last_n_summary(org, time_range),
    )?;

    let truncated = targets.len() > ROW_LIMIT;
    if truncated {
        targets.truncate(ROW_LIMIT);
    }

    let metrics_by_target: HashMap<Uuid, DashboardMetrics> =
        rollup.into_iter().map(|m| (m.target_id, m)).collect();
    let spark_by_target = group_sparks(&spark_rows, spark_from);

    let mut sample_total_ms: u64 = 0;
    let mut sample_total_n: u64 = 0;
    let rows: Vec<DashboardRow> = targets
        .into_iter()
        .map(|t| {
            let (kind, address) = describe_check(&t.check);
            let metrics = metrics_by_target.get(&t.id);
            let spark = spark_by_target
                .get(&t.id)
                .cloned()
                .unwrap_or_else(|| vec![None; SPARK_BUCKETS]);
            if let Some(m) = metrics {
                sample_total_ms =
                    sample_total_ms.saturating_add((m.avg_ms as u64).saturating_mul(m.samples));
                sample_total_n = sample_total_n.saturating_add(m.samples);
            }
            DashboardRow::build(t.id, t.name, kind, address, t.enabled, metrics, spark)
        })
        .collect();

    let kpis = DashboardKpis {
        uptime_pct_label: pct_label(checks_total, checks_up),
        avg_response_ms_label: avg_response_label(sample_total_ms, sample_total_n),
        checks_label: format_count(checks_total),
        incidents,
    };

    let matches = rows.len();
    Ok(DashboardSnapshot {
        rows: Arc::from(rows.into_boxed_slice()),
        kpis,
        matches,
        truncated,
    })
}

fn range_span(key: &'static str) -> Duration {
    match key {
        "7d" => Duration::days(7),
        "30d" => Duration::days(30),
        "90d" => Duration::days(90),
        _ => Duration::hours(24),
    }
}

fn pct_label(total: u64, up: u64) -> String {
    if total == 0 {
        return "—".into();
    }
    format!("{:.2}%", (up as f64 / total as f64) * 100.0)
}

fn avg_response_label(sum_ms: u64, n: u64) -> String {
    if n == 0 {
        return "—".into();
    }
    let avg = sum_ms as f64 / n as f64;
    format!("{} ms", avg.round() as u64)
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
    fn build(
        id: Uuid,
        name: String,
        kind: &'static str,
        address: String,
        enabled: bool,
        metrics: Option<&DashboardMetrics>,
        spark: Vec<Option<f32>>,
    ) -> Self {
        let (p50_label, p95_label, err_pct_label, uptime_pct_label, last_status, samples) =
            match metrics {
                Some(m) if m.samples > 0 => {
                    let err_pct = ((m.samples - m.up) as f64 / m.samples as f64) * 100.0;
                    let uptime_pct = 100.0 - err_pct;
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

fn status_label(raw: &str) -> &'static str {
    match raw {
        "up" => "up",
        "down" => "down",
        "degraded" => "degraded",
        "error" => "error",
        _ => "",
    }
}

/// Per-row sparkline rendered into a `160×22` viewport (matches V3
/// design spec). Returns `(line_path, fill_path, baseline_y)`:
///   - `line_path`: polyline `M…L…`. Auto-scales to the row's own
///     min/max so a fast monitor shows shape, not a flat line crushed
///     by a slow neighbour. `M`-restart on each `None` so interior
///     gaps break the line cleanly.
///   - `fill_path`: closed area under the line, used for the soft
///     tint. Built per contiguous segment of ≥2 points — each gap
///     splits into its own closed sub-path so the fill never bridges
///     a missing minute. Empty when no segment qualifies.
///   - `baseline_y`: y-coord for the dashed "no data" line used when
///     the row has zero samples.
fn render_spark_path(spark: &[Option<f32>]) -> (String, String, u32) {
    const W: f32 = 160.0;
    const H: f32 = 22.0;
    let baseline_y = (H / 2.0).round() as u32;
    // Treat NaN/Inf as missing — CH `avgMerge` returns NaN for empty
    // groups and a single non-finite poisons min/max, producing
    // `"M NaN NaN"` paths that browsers silently drop.
    let finite = |o: &Option<f32>| o.filter(|v| v.is_finite());
    let present: Vec<f32> = spark.iter().filter_map(finite).collect();
    if present.is_empty() {
        return (String::new(), String::new(), baseline_y);
    }
    let min = present.iter().copied().fold(f32::INFINITY, f32::min);
    let max = present.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = (max - min).max(1.0);

    let first = spark.iter().position(|s| finite(s).is_some()).unwrap_or(0);
    let last = spark.iter().rposition(|s| finite(s).is_some()).unwrap_or(0);
    let active_span = (last - first) as f32;
    let step = if active_span > 0.0 {
        W / active_span
    } else {
        0.0
    };

    let mut line = String::with_capacity(spark.len() * 14);
    let mut fill = String::with_capacity(spark.len() * 14);
    let mut segment: Vec<(f32, f32)> = Vec::with_capacity(present.len());
    let mut pen_down = false;

    let flush_segment = |fill: &mut String, seg: &mut Vec<(f32, f32)>| {
        if seg.len() >= 2 {
            let (fx, fy) = seg[0];
            let (lx, _) = *seg.last().unwrap();
            write!(fill, "M{fx:.1} {fy:.1}").unwrap();
            for &(x, y) in &seg[1..] {
                write!(fill, " L{x:.1} {y:.1}").unwrap();
            }
            write!(fill, " L{lx:.1} {H:.1} L{fx:.1} {H:.1} Z").unwrap();
        }
        seg.clear();
    };

    for (i, slot) in spark.iter().enumerate().skip(first).take(last - first + 1) {
        match finite(slot) {
            None => {
                pen_down = false;
                flush_segment(&mut fill, &mut segment);
            }
            Some(v) => {
                let x = if active_span > 0.0 {
                    step * (i - first) as f32
                } else {
                    W / 2.0
                };
                let y = (1.0 - (v - min) / span) * H;
                if pen_down {
                    write!(line, " L{x:.1} {y:.1}").unwrap();
                } else {
                    write!(line, "M{x:.1} {y:.1}").unwrap();
                    pen_down = true;
                }
                segment.push((x, y));
            }
        }
    }
    flush_segment(&mut fill, &mut segment);

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
            incidents: 3,
        }
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

    fn sample_page() -> DashboardPage {
        let rows = vec![sample_row("api", "up"), sample_row("worker", "degraded")];
        DashboardPage {
            active_tab: "dashboard",
            range: "24h",
            range_options: build_range_options("24h", &RANGE_KEYS),
            kpis: sample_kpis(),
            rows: Arc::from(rows.into_boxed_slice()),
            matches: 2,
            truncated: false,
            onboarding: false,
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
            kpis: sample_kpis(),
            rows: Arc::from(vec![sample_row("api", "up")].into_boxed_slice()),
            matches: 1,
            truncated: false,
        };
        let html = partial.render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(!html.contains("<nav"));
        assert!(html.contains(r#"id="dashboard-table""#));
        assert!(html.contains("99.92%"));
    }

    #[test]
    fn onboarding_state_skips_table() {
        let page = DashboardPage {
            active_tab: "dashboard",
            range: "24h",
            range_options: build_range_options("24h", &RANGE_KEYS),
            kpis: DashboardKpis {
                uptime_pct_label: "—".into(),
                avg_response_ms_label: "—".into(),
                checks_label: "0".into(),
                incidents: 0,
            },
            rows: Arc::from(Vec::<DashboardRow>::new().into_boxed_slice()),
            matches: 0,
            truncated: false,
            onboarding: true,
        };
        let html = page.render().unwrap();
        assert!(html.contains("No monitors yet"));
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

    #[test]
    fn render_spark_path_breaks_on_none() {
        let path = render_spark_path(&[Some(1.0), Some(2.0), None, Some(3.0)]).0;
        assert!(path.starts_with('M'));
        // Two `M` segments — once at start, once after the gap.
        assert_eq!(path.matches('M').count(), 2);
    }

    #[test]
    fn render_spark_path_empty_when_no_data() {
        assert_eq!(render_spark_path(&vec![None; 60]).0, "");
    }
}
