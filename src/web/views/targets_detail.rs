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
use crate::api::types::AvailabilityBucket;
use crate::app::AppState;
use crate::domain::{
    CheckResult, CheckSpec, Incident, OrgId, confirmed_downtime_secs, uptime_pct_from_downtime,
};
use crate::error::AppError;
use crate::storage::{ClampedRange, TimeRange, UptimeStats, rollup_bucket_secs};
use crate::web::error::{WebError, WebResult};
use crate::web::filters;
use crate::web::views::dashboard::{
    KpiDelta, Polarity, count_delta, render_spark_path_domain, ribbon_class, uptime_pp_delta,
};
use crate::web::views::region_display::{LabeledRegion, labeled_regions};
use crate::web::views::{
    RangeOption, build_range_options, describe_check, fmt_human, fmt_ts, resolve_range_key,
};
use crate::web::{AuthedBrowser, CurrentOrg};

// Recent rows for the share page's results table (the owner view dropped its
// table for the ribbon). 60 ≈ the last hour at a 1-minute interval.
const RESULTS_PAGE_LIMIT: usize = 60;
// Confirmed incidents over the longest window (90d) are few; this bounds the
// downtime read without truncating a realistic count.
const CONFIRMED_UPTIME_INCIDENT_CAP: usize = 2000;
pub(crate) const RANGE_KEYS: [&str; 4] = ["1h", "24h", "7d", "30d"];
pub(crate) const DEFAULT_RANGE: &str = "24h";
// Decoupled from the user's chart range so the header badge reflects the
// monitor's actual current state, not "no data" when the user picked 1h
// but the last check was 2h ago.
const LAST_RESULT_WINDOW_DAYS: i64 = 7;
// Target point count for the uptime sparkline; bucket width derives from it.
const SPARK_SEGMENTS: i64 = 48;

pub(crate) const SUBTAB_MONITOR: &str = "monitor";
pub(crate) const SUBTAB_INCIDENTS: &str = "incidents";

pub(crate) const INCIDENT_RANGE_KEYS: [&str; 4] = ["24h", "7d", "30d", "90d"];
pub(crate) const INCIDENT_DEFAULT_RANGE: &str = "30d";
const INCIDENTS_PAGE_LIMIT: usize = 100;

pub(crate) fn ongoing_from_status(status: &str) -> usize {
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
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Clone)]
pub struct ResultRow {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub status: &'static str,
    pub duration_ms: u32,
    pub response_code: String,
    pub error: String,
    pub dns_ms: Option<u16>,
    pub connect_ms: Option<u16>,
    pub tls_ms: Option<u16>,
    pub ttfb_ms: Option<u16>,
    /// Probe region; `Some` only where the row is region-tagged (the drill
    /// drawer). `None` on the region-agnostic recent-results table.
    pub region: Option<String>,
}

impl ResultRow {
    /// Row tagged with the region it ran in; an empty region label reads as `None`.
    fn with_region(region: String, r: CheckResult) -> Self {
        Self {
            region: (!region.is_empty()).then_some(region),
            ..Self::from(r)
        }
    }
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
    pub enabled: bool,
    pub last_status: &'static str,
    pub uptime: Arc<UptimeStatsView>,
    pub kpi: Arc<KpiTrend>,
    pub last_at_iso: Arc<str>,
    /// Carried so the self-rearming live poll keeps the active region filter.
    pub selected_region: Option<String>,
    /// Per-bucket status ribbon, re-rendered here so the 60s poll keeps the
    /// newest cell current.
    pub segments: Arc<[StatusSeg]>,
    /// Marks the ribbon include as an out-of-band swap on the live response.
    pub ribbon_oob: bool,
}

/// Rows for the ribbon drill drawer: a page of raw checks over a bucket window,
/// each tagged with its region. Renders the shared recent-results row partial
/// with the region column shown.
#[derive(Template, WebTemplate)]
#[template(path = "targets/partials/detail_live_rows.html")]
pub struct DetailCheckRows {
    pub results: Arc<[ResultRow]>,
    pub show_region: bool,
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
    /// `terraform`/`api` chip for externally-managed monitors; `None` (UI) hides it.
    pub managed_by: Option<&'static str>,
    /// Count of live (non-revoked) share links; drives the header "shared" chip.
    pub share_count: usize,
    pub last_status: &'static str,
    pub last_at_iso: String,
    pub incidents: Vec<IncidentRow>,
    pub incidents_has_more: bool,
    /// URL prefix incidents_tab.js appends `/results` to for the timeline
    /// drawer (`/api/v1/targets/{id}`).
    pub results_base: String,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub range_base_path: String,
    pub from_iso: String,
    pub to_iso: String,
    pub from_human: String,
    pub to_human: String,
    /// Always `None` here — the incidents tab is not region-filtered yet; the
    /// field exists so the shared range-pills partial type-checks.
    pub selected_region: Option<String>,
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
    /// `terraform`/`api` chip for externally-managed monitors; `None` (UI) hides it.
    pub managed_by: Option<&'static str>,
    /// Count of live (non-revoked) share links; drives the header "shared" chip.
    pub share_count: usize,
    pub last_status: &'static str,
    /// ISO 8601 timestamp of the most recent check, "" when none. Drives
    /// the client-side "checked Ns ago · next in Ns" ticker.
    pub last_at_iso: Arc<str>,
    pub uptime: Arc<UptimeStatsView>,
    pub kpi: Arc<KpiTrend>,
    /// Per-bucket status strip over the selected range, rendered under the header.
    pub segments: Arc<[StatusSeg]>,
    /// Ribbon include renders in place (not an OOB swap) on the full page.
    pub ribbon_oob: bool,
    /// Registrable domain a `domain_expiry` monitor queries; `None` otherwise.
    pub registered_domain: Option<String>,
    pub config_json: String,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub range_base_path: String,
    pub from_iso: String,
    pub to_iso: String,
    pub from_human: String,
    pub to_human: String,
    /// Distinct regions this org's targets run in; drives the region selector,
    /// rendered only when there is more than one.
    pub regions: Vec<LabeledRegion>,
    pub selected_region: Option<String>,
    /// Per-region rollup rows; empty for single-region orgs (table hidden).
    pub region_breakdown: Vec<RegionBreakdownRow>,
    /// Ping-URL card for heartbeat monitors; `None` for every other kind.
    /// Shares the API handler's projection so the two surfaces can't diverge.
    pub heartbeat: Option<crate::api::handlers::targets::HeartbeatInfo>,
}

/// One row of the per-region breakdown table on the monitor detail page.
pub struct RegionBreakdownRow {
    pub region_label: String,
    pub uptime_label: String,
    pub p50_label: String,
    pub p95_label: String,
    /// "" when the region has no samples in the range.
    pub last_status: String,
    /// Marks the row matching the active region filter.
    pub selected: bool,
}

impl RegionBreakdownRow {
    fn from_rollup(
        r: crate::api::types::RegionRollup,
        selected_region: Option<&str>,
        catalog: &[crate::storage::RegionOption],
        live_regions: Option<&std::collections::HashSet<String>>,
    ) -> Self {
        let uptime_label = super::dashboard::pct_label(r.samples, r.up);
        let selected = selected_region == Some(r.region.as_str());
        let region_label = catalog
            .iter()
            .find(|c| c.id == r.region)
            .map(crate::web::views::region_display::region_label)
            .unwrap_or_else(|| r.region.clone());
        // No live probe overrides the stale last status with grey "no data".
        // `None` = liveness unknown (query failed); leave the status untouched
        // rather than greying every region on a transient blip.
        let last_status = match live_regions {
            Some(live) if !live.contains(&r.region) => "no_data".to_string(),
            _ => r.last_status,
        };
        Self {
            selected,
            uptime_label,
            p50_label: format!("{} ms", r.p50_ms),
            p95_label: format!("{} ms", r.p95_ms),
            last_status,
            region_label,
        }
    }
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
            uptime_pct: fmt_uptime_pct(s.uptime_pct),
        }
    }
}

/// Two-decimal uptime with trailing zeros trimmed: `100`, `99.98`, `99.9`.
fn fmt_uptime_pct(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Sparkline + Δ-vs-prior chips for the KPI strip, shared by the owner detail
/// and public share pages. `Default` renders no spark and no deltas — used
/// when there is no prior window to compare against.
#[derive(Default)]
pub struct KpiTrend {
    pub spark_path: String,
    pub spark_fill: String,
    pub uptime_delta: Option<KpiDelta>,
    pub up_delta: Option<KpiDelta>,
    pub down_delta: Option<KpiDelta>,
    pub error_delta: Option<KpiDelta>,
}

/// ISO and human-readable strings for a `(from, to)` window. Both
/// detail-page templates render the same four fields in the chrome
/// (range pill caption, time inputs); precomputing once keeps the
/// template free of filter chains for one-shot values.
pub(crate) struct WindowLabels {
    pub from_iso: String,
    pub to_iso: String,
    pub from_human: String,
    pub to_human: String,
}

impl WindowLabels {
    pub(crate) fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
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
    pub kpi: Arc<KpiTrend>,
    /// Recent rows for the public share page's results table. The owner detail
    /// dropped its table for the ribbon, but both surfaces share this loader.
    pub result_rows: Arc<[ResultRow]>,
    pub results_has_more: bool,
    pub last_status: &'static str,
    pub last_at_iso: Arc<str>,
    /// Per-bucket status strip over the window; drives the timeline under the header.
    pub segments: Arc<[StatusSeg]>,
}

/// Cached front door for [`load_live_data`]. Returns a moka-shared
/// `Arc<LiveData>` keyed on `(org, target_id, range_key)`. Preset
/// ranges are cached for 5s; ad-hoc `from`/`to` windows skip the cache
/// (one-off queries shouldn't pollute the shared bucket). Both the
/// full-page `index` and the htmx `live_partial` go through here so a
/// burst of either request type collapses to a single CH round-trip.
pub(crate) async fn load_live_data_cached(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
    range_key: &'static str,
    custom_from: Option<DateTime<Utc>>,
    custom_to: Option<DateTime<Utc>>,
    region: Option<&str>,
) -> WebResult<Arc<LiveData>> {
    // A region-filtered view skips the shared cache (selection is rare, not
    // worth widening the key) — same call as the custom-window path.
    let cacheable = custom_from.is_none() && custom_to.is_none() && region.is_none();
    if cacheable {
        let key = (org, target_id, range_key);
        if let Some(data) = state.live_data_cache.get(&key) {
            return Ok(data);
        }
        let (from, to) = resolve_window(range_key, custom_from, custom_to);
        let data = Arc::new(load_live_data(state, org, target_id, from, to, region).await?);
        state.live_data_cache.insert(key, data.clone());
        Ok(data)
    } else {
        let (from, to) = resolve_window(range_key, custom_from, custom_to);
        Ok(Arc::new(
            load_live_data(state, org, target_id, from, to, region).await?,
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
    // Current-status badge, not history browsing — exempt from the plan clamp.
    let row = state
        .results_store
        .list_results(
            org,
            target_id,
            ClampedRange::unclamped(TimeRange { from, to }),
            1,
            0,
            None,
        )
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
    region: Option<&str>,
) -> WebResult<LiveData> {
    // Cap `to` at now: a monitor has no future samples, and an unbounded `to`
    // would let the span (hence the spark series length) grow without limit.
    let time_range = state
        .quotas
        .clamp_raw(
            org,
            TimeRange {
                from,
                to: to.min(Utc::now()),
            },
        )
        .await?;
    let span = time_range.to - time_range.from;
    let prior_range = state
        .quotas
        .clamp_raw(
            org,
            TimeRange {
                from: time_range.from - span,
                to: time_range.from,
            },
        )
        .await?;
    let spark_bucket_seconds = (span.num_seconds() / SPARK_SEGMENTS).max(60) as u32;
    // Confirmed downtime drives uptime only for the all-regions view; a region
    // filter keeps the raw per-region sample rate and needs no incident reads.
    let confirmed = region.is_none();

    let (mut uptime, mut results, avail, prior, cur_incidents, prior_incidents) = tokio::try_join!(
        state
            .results_store
            .uptime(org, target_id, time_range, region),
        state.results_store.list_results(
            org,
            target_id,
            time_range,
            RESULTS_PAGE_LIMIT + 1,
            0,
            region
        ),
        state.results_store.availability_buckets(
            org,
            target_id,
            time_range,
            spark_bucket_seconds,
            region
        ),
        state
            .results_store
            .uptime(org, target_id, prior_range, region),
        confirmed_incidents(state, org, target_id, time_range, confirmed),
        confirmed_incidents(state, org, target_id, prior_range, confirmed),
    )?;
    if confirmed && uptime.total > 0 {
        uptime.uptime_pct = confirmed_uptime_pct(&cur_incidents, time_range);
    }
    let kpi = build_kpi_trend(KpiInputs {
        current: &uptime,
        prior: &prior,
        cur_incidents: &cur_incidents,
        prior_incidents: &prior_incidents,
        avail: &avail,
        range: time_range,
        prior_range,
        spark_bucket_seconds,
        confirmed,
    });
    // The strip shows raw per-bucket check outcomes so short error/degraded
    // patches stay visible, even when they never breached the incident
    // threshold the confirmed headline uptime is measured against. Empty
    // buckets read as grey gaps, not green.
    let segments: Arc<[StatusSeg]> = status_segments(
        &bucket_counts(&avail, time_range, spark_bucket_seconds),
        time_range,
        spark_bucket_seconds,
    )
    .into();
    let results_has_more = results.len() > RESULTS_PAGE_LIMIT;
    if results_has_more {
        results.truncate(RESULTS_PAGE_LIMIT);
    }

    // Last-known status when the window is empty — current-status, not history.
    let latest_outside_window = if results.is_empty()
        && let Some(window) = wider_status_window(from, to)
    {
        state
            .results_store
            .list_results(
                org,
                target_id,
                ClampedRange::unclamped(window),
                1,
                0,
                region,
            )
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
        kpi: Arc::new(kpi),
        result_rows,
        results_has_more,
        last_status,
        last_at_iso,
        segments,
    })
}

/// Confirmed incidents for the downtime read, or an empty vec (no DB hit) when
/// the view is region-filtered and uses the raw rate instead. Kept separate so
/// `load_live_data` can fan out current + prior reads in one `try_join`.
async fn confirmed_incidents(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
    range: ClampedRange,
    confirmed: bool,
) -> crate::error::Result<Vec<Incident>> {
    if !confirmed {
        return Ok(Vec::new());
    }
    state
        .incident_narration_store
        .list_for_target(
            org,
            target_id,
            range.inner(),
            CONFIRMED_UPTIME_INCIDENT_CAP,
            0,
            false,
        )
        .await
}

fn confirmed_uptime_pct(incidents: &[Incident], range: ClampedRange) -> f64 {
    let down = confirmed_downtime_secs(incidents, range.from, range.to, Utc::now());
    uptime_pct_from_downtime(down, (range.to - range.from).num_seconds())
}

/// Prefetched reads `build_kpi_trend` turns into the KPI strip's spark + deltas.
struct KpiInputs<'a> {
    current: &'a UptimeStats,
    prior: &'a UptimeStats,
    cur_incidents: &'a [Incident],
    prior_incidents: &'a [Incident],
    avail: &'a [AvailabilityBucket],
    range: ClampedRange,
    prior_range: ClampedRange,
    spark_bucket_seconds: u32,
    confirmed: bool,
}

/// Uptime sparkline plus Δ-vs-prior chips. Both mirror the displayed uptime %'s
/// source — confirmed downtime for the all-regions view, raw rate when
/// region-filtered — so neither the spark nor a chip can contradict the figure.
/// The prior window is the same span immediately before `range`.
fn build_kpi_trend(inp: KpiInputs) -> KpiTrend {
    let (spark_path, spark_fill) = build_availability_spark(
        inp.avail,
        inp.cur_incidents,
        inp.range,
        inp.spark_bucket_seconds,
        inp.confirmed,
    );
    // Deltas need data in the current window and a full-length prior window: an
    // empty current window would read as a 100pp drop, and a prior truncated by
    // the retention floor would skew the raw counts against a shorter span.
    let comparable = inp.current.total > 0
        && inp.prior.total > 0
        && (inp.prior_range.to - inp.prior_range.from) >= (inp.range.to - inp.range.from);
    if !comparable {
        return KpiTrend {
            spark_path,
            spark_fill,
            ..Default::default()
        };
    }
    let prior_pct = if inp.confirmed {
        confirmed_uptime_pct(inp.prior_incidents, inp.prior_range)
    } else {
        inp.prior.uptime_pct
    };
    KpiTrend {
        spark_path,
        spark_fill,
        uptime_delta: Some(uptime_pp_delta(inp.current.uptime_pct, prior_pct)),
        up_delta: Some(count_delta(
            inp.current.up,
            inp.prior.up,
            Polarity::HigherIsBetter,
        )),
        down_delta: Some(count_delta(
            inp.current.down,
            inp.prior.down,
            Polarity::LowerIsBetter,
        )),
        error_delta: Some(count_delta(
            inp.current.error,
            inp.prior.error,
            Polarity::LowerIsBetter,
        )),
    }
}

/// Builds the uptime sparkline over a fixed 0–100 domain (healthy sits flat at
/// the top, dips notch down). All-regions decomposes the confirmed headline:
/// every bucket is `1 − confirmed-incident-overlap`, measured the same way as
/// the figure beside it (whole window, trailing bucket up to `now`), so the line
/// can't disagree with it. Region-filtered has no confirmed model — it falls
/// back to the raw up-ratio, leaving sample gaps as `None`.
fn build_availability_spark(
    avail: &[AvailabilityBucket],
    incidents: &[Incident],
    range: ClampedRange,
    bucket_seconds: u32,
    confirmed: bool,
) -> (String, String) {
    let series = availability_series(avail, incidents, range, bucket_seconds, confirmed);
    let (path, fill, _) = render_spark_path_domain(&series, 0.0, 100.0);
    (path, fill)
}

/// Per-bucket availability over the window as a 0–100 series (`None` = no
/// samples). Shared by the KPI sparkline and the status strip so both read the
/// same model as the headline: confirmed-incident overlap for the all-regions
/// view, raw up-ratio when region-filtered.
fn availability_series(
    avail: &[AvailabilityBucket],
    incidents: &[Incident],
    range: ClampedRange,
    bucket_seconds: u32,
    confirmed: bool,
) -> Vec<Option<f32>> {
    let bucket = i64::from(rollup_bucket_secs(bucket_seconds));
    let from_grid = range.from.timestamp().div_euclid(bucket) * bucket;
    let n = (((range.to.timestamp() - from_grid) + bucket - 1) / bucket).max(1) as usize;
    let mut series = vec![None; n];
    if confirmed {
        let now = Utc::now();
        let now_ts = now.timestamp();
        for (i, slot) in series.iter_mut().enumerate() {
            let start_ts = from_grid + i as i64 * bucket;
            let end_ts = (start_ts + bucket).min(now_ts);
            let window = end_ts - start_ts;
            if window <= 0 {
                continue;
            }
            let start = DateTime::from_timestamp(start_ts, 0).unwrap_or(range.from);
            let end = DateTime::from_timestamp(end_ts, 0).unwrap_or(range.to);
            let down = confirmed_downtime_secs(incidents, start, end, now);
            *slot = Some(uptime_pct_from_downtime(down, window) as f32);
        }
    } else {
        for b in avail {
            if b.total == 0 {
                continue;
            }
            let slot = (b.bucket_ts - from_grid).div_euclid(bucket);
            if slot >= 0 && (slot as usize) < n {
                series[slot as usize] = Some((b.up as f32 / b.total as f32) * 100.0);
            }
        }
    }
    series
}

/// One cell of the uptime ribbon under the header. Mirrors the dashboard's
/// fleet ribbon so both reuse `.dashboard-ribbon__seg` and its tooltip JS.
pub struct StatusSeg {
    /// `op` | `deg` | `maj` | `none` — drives the `dashboard-ribbon__seg--*` class.
    pub class: &'static str,
    /// Bucket start (`%H:%M`) and uptime figure, shown in the hover tooltip.
    pub time: String,
    pub stat: String,
    /// ISO bounds of the bucket; a failing cell drills the drawer to this window.
    pub from_iso: String,
    pub to_iso: String,
    /// Check counts in the bucket; the drawer shows "{bad} of {total} failing"
    /// so a wide window's scale is known without fetching every row.
    pub total: u64,
    pub bad: u64,
}

/// Fixed-length `(total, up)` per bucket from the raw availability rollup, on the
/// same grid the ribbon draws. Feeds both the up-ratio tint and the drawer's
/// scale count, so one pass over `avail` serves both.
fn bucket_counts(
    avail: &[AvailabilityBucket],
    range: ClampedRange,
    bucket_seconds: u32,
) -> Vec<(u64, u64)> {
    let bucket = i64::from(rollup_bucket_secs(bucket_seconds));
    let from_grid = range.from.timestamp().div_euclid(bucket) * bucket;
    let n = (((range.to.timestamp() - from_grid) + bucket - 1) / bucket).max(1) as usize;
    let mut counts = vec![(0u64, 0u64); n];
    for b in avail {
        let slot = (b.bucket_ts - from_grid).div_euclid(bucket);
        if slot >= 0 && (slot as usize) < n {
            let c = &mut counts[slot as usize];
            c.0 += b.total;
            c.1 += b.up;
        }
    }
    counts
}

/// Maps per-bucket `(total, up)` counts to ribbon cells, classified by the same
/// `ribbon_class` thresholds the dashboard fleet ribbon uses. A bucket with no
/// samples is `none` (hatched) so a paused or young monitor reads as a gap, not
/// green. `time`/`stat` populate the tooltip; `total`/`bad` size the drawer.
fn status_segments(
    counts: &[(u64, u64)],
    range: ClampedRange,
    bucket_seconds: u32,
) -> Vec<StatusSeg> {
    let bucket = i64::from(rollup_bucket_secs(bucket_seconds));
    let from_grid = range.from.timestamp().div_euclid(bucket) * bucket;
    let to_ts = range.to.timestamp();
    counts
        .iter()
        .enumerate()
        .map(|(i, &(total, up))| {
            let start_ts = from_grid + i as i64 * bucket;
            let end_ts = (start_ts + bucket).min(to_ts).max(start_ts);
            let start = DateTime::from_timestamp(start_ts, 0).unwrap_or(range.from);
            let end = DateTime::from_timestamp(end_ts, 0).unwrap_or(range.to);
            let time = start.format("%H:%M").to_string();
            // The first cell's grid start precedes range.from; drill from range.from
            // so the drawer never lists rows the cell's counts never included.
            let drill_from = DateTime::from_timestamp(start_ts.max(range.from.timestamp()), 0)
                .unwrap_or(range.from);
            let from_iso = fmt_ts(drill_from);
            let to_iso = fmt_ts(end);
            let bad = total.saturating_sub(up);
            if total == 0 {
                StatusSeg {
                    class: "none",
                    time,
                    stat: "no data".into(),
                    from_iso,
                    to_iso,
                    total,
                    bad,
                }
            } else {
                let pct = (up as f64 / total as f64) * 100.0;
                StatusSeg {
                    class: ribbon_class(pct),
                    time,
                    stat: format!("{pct:.1}%"),
                    from_iso,
                    to_iso,
                    total,
                    bad,
                }
            }
        })
        .collect()
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
    let region_ids = state.regions_for_org(org).await?;
    let selected_region = super::dashboard::resolve_region(params.region, &region_ids);
    let catalog = state.regions_detailed().await?;
    let live = load_live_data_cached(
        &state,
        org,
        target.id,
        range_key,
        params.from,
        params.to,
        selected_region.as_deref(),
    )
    .await?;
    let region_breakdown = if region_ids.len() > 1 && !target.check.is_passive() {
        let live: Option<std::collections::HashSet<String>> = state
            .silence_store
            .live_regions(state.cfg.operator.agent_stale_after_secs)
            .await
            .ok()
            .map(|v| v.into_iter().collect());
        state
            .results_store
            .region_breakdown(org, target.id, TimeRange { from, to })
            .await?
            .into_iter()
            .map(|r| {
                RegionBreakdownRow::from_rollup(
                    r,
                    selected_region.as_deref(),
                    &catalog,
                    live.as_ref(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let ongoing_count = ongoing_from_status(live.last_status);
    let registered_domain = match &target.check {
        CheckSpec::DomainExpiry(d) => d.reduced_domain_hint(),
        _ => None,
    };
    let config_json = config_json_with_derived(&target.check, registered_domain.as_deref())?;
    let (kind, address) = describe_check(&target.check);
    let share_count = state
        .monitor_share_store
        .count_active_for_target(org, target.id)
        .await? as usize;
    let heartbeat = if target.check.is_passive() {
        Some(crate::api::handlers::targets::heartbeat_info(&state, org, target.id).await?)
    } else {
        None
    };
    // Passive kinds have no probe region, so no region selector.
    let regions = if target.check.is_passive() {
        Vec::new()
    } else {
        labeled_regions(&catalog, region_ids)
    };

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
        managed_by: target.write_source.managed_label(),
        share_count,
        last_status: live.last_status,
        last_at_iso: Arc::clone(&live.last_at_iso),
        uptime: Arc::clone(&live.uptime),
        kpi: Arc::clone(&live.kpi),
        segments: Arc::clone(&live.segments),
        ribbon_oob: false,
        registered_domain,
        config_json,
        range: range_key,
        range_options: build_range_options(range_key, &RANGE_KEYS),
        range_base_path: format!("/targets/{}", target.id),
        from_iso: labels.from_iso,
        to_iso: labels.to_iso,
        from_human: labels.from_human,
        to_human: labels.to_human,
        regions,
        selected_region,
        region_breakdown,
        heartbeat,
    })
}

#[derive(Debug, Deserialize)]
pub struct CheckRowsParams {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    #[serde(default)]
    pub offset: usize,
    /// Mirrors the detail view's region filter so the drawer lists the same
    /// region the ribbon cell's counts were measured over.
    pub region: Option<String>,
}

// One 30-row page is plenty per drill; the drawer pages with `offset` for more.
const CHECK_ROWS_PAGE: usize = 30;

/// HTML rows for the ribbon drill drawer: the failing checks over a bucket
/// window, each tagged with its region, rendered with the shared recent-results
/// row partial. Paginates by `offset`; `X-Sm-Has-More` tells the drawer whether
/// to keep its "load more" control. A wide bucket can hold thousands of failures,
/// so the caller only ever pulls one bounded page at a time.
pub async fn check_rows(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<CheckRowsParams>,
) -> WebResult<Response> {
    // Cloak an unknown/foreign id behind the same 404 the other detail reads use.
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "monitor not found"))?;

    let range = state
        .quotas
        .clamp_raw(
            org,
            TimeRange {
                from: params.from,
                to: params.to,
            },
        )
        .await?;
    // The drawer only opens on a failing cell, so list only the failing checks —
    // success rows would bury them. `region` matches the ribbon's filter. Counts
    // come from the rollup and these rows from raw, so they can diverge at a TTL
    // or bucket edge; the count is the headline, not a row total.
    let mut rows = state
        .results_store
        .list_failures_by_region(
            org,
            target.id,
            range,
            CHECK_ROWS_PAGE + 1,
            params.offset,
            params.region.as_deref(),
        )
        .await?;
    let has_more = rows.len() > CHECK_ROWS_PAGE;
    if has_more {
        rows.truncate(CHECK_ROWS_PAGE);
    }
    let results: Arc<[ResultRow]> = rows
        .into_iter()
        .map(|(region, r)| ResultRow::with_region(region, r))
        .collect::<Vec<_>>()
        .into();

    let rendered = DetailCheckRows {
        results,
        show_region: true,
    }
    .render()
    .map_err(|e| AppError::Other(anyhow::anyhow!(e)))?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (
                header::HeaderName::from_static("x-sm-has-more"),
                if has_more { "true" } else { "false" },
            ),
        ],
        rendered,
    )
        .into_response())
}

/// Pretty check config for the panel, with a read-only `registered_domain`
/// added when a `domain_expiry` input reduces, so the displayed input isn't
/// rewritten.
fn config_json_with_derived(
    check: &CheckSpec,
    registered_domain: Option<&str>,
) -> Result<String, AppError> {
    let mut value = serde_json::to_value(check).map_err(|e| AppError::Other(anyhow::anyhow!(e)))?;
    if let (Some(rd), Some(object)) = (registered_domain, value.as_object_mut()) {
        object.insert("registered_domain".into(), rd.into());
    }
    serde_json::to_string_pretty(&value).map_err(|e| AppError::Other(anyhow::anyhow!(e)))
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
    // The poll only echoes the region into its own URL — it doesn't render the
    // selector — so it trusts the region the full page already validated rather
    // than re-running regions_for_org every tick. An unknown value just filters
    // to empty (org-scoped), corrected on the next full load.
    let selected_region = params.region;
    let live = load_live_data_cached(
        &state,
        org,
        target.id,
        range_key,
        params.from,
        params.to,
        selected_region.as_deref(),
    )
    .await?;

    let page = DetailLive {
        id: target.id.to_string(),
        name: target.name,
        range: range_key,
        enabled: target.enabled,
        last_status: live.last_status,
        uptime: Arc::clone(&live.uptime),
        kpi: Arc::clone(&live.kpi),
        last_at_iso: Arc::clone(&live.last_at_iso),
        selected_region,
        segments: Arc::clone(&live.segments),
        ribbon_oob: true,
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
pub(crate) fn resolve_window(
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
            timestamp: r.timestamp,
            status: r.status.as_str(),
            duration_ms: r.duration_ms,
            response_code: r.response_code.map(|c| c.to_string()).unwrap_or_default(),
            error: r
                .error
                .as_deref()
                .map(fmt_error_display)
                .unwrap_or_default(),
            dns_ms: r.dns_ms,
            connect_ms: r.connect_ms,
            tls_ms: r.tls_ms,
            ttfb_ms: r.ttfb_ms,
            region: None,
        }
    }
}

pub(crate) use crate::domain::humanize_check_error as fmt_error_display;

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
            error_sample: inc
                .error_sample
                .as_deref()
                .map(fmt_error_display)
                .unwrap_or_default(),
            ongoing,
        }
    }
}

/// The incidents-tab data for one monitor: the header badge's live status, the
/// page of incident rows, and the derived ongoing count. Loaded by
/// [`load_incidents_data`] so the operator incidents page and the public share
/// view render the same dataset from one query graph.
pub(crate) struct IncidentsData {
    pub last_status: &'static str,
    pub last_at_iso: String,
    pub rows: Vec<IncidentRow>,
    pub has_more: bool,
    pub ongoing_count: usize,
}

/// Load the incidents tab's dataset for `(org, target_id)` over `time_range`
/// from the confirmed materialised incidents.
pub(crate) async fn load_incidents_data(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
    time_range: TimeRange,
) -> WebResult<IncidentsData> {
    let range = state.quotas.clamp_raw(org, time_range).await?;
    // The incidents tab needs only the badge's `last_status` from the live
    // region — not the uptime stats or 60 recent rows. Probe one row instead of
    // running the full live loader; a 90d preset would otherwise scan the entire
    // window for the same single field.
    let ((last_status, last_at_iso), mut incidents) =
        tokio::try_join!(latest_status_probe(state, org, target_id), async {
            state
                .incident_narration_store
                .list_for_target(
                    org,
                    target_id,
                    range.inner(),
                    INCIDENTS_PAGE_LIMIT + 1,
                    0,
                    false,
                )
                .await
                .map_err(WebError::from)
        },)?;

    let has_more = incidents.len() > INCIDENTS_PAGE_LIMIT;
    if has_more {
        incidents.truncate(INCIDENTS_PAGE_LIMIT);
    }
    // Tab badge: prefer the live-status signal so the count matches what the
    // Monitor tab shows. Fall back to counting open incidents in the list when
    // the user narrowed to a window where last_status is stale.
    let ongoing_count = ongoing_from_status(last_status)
        .max(incidents.iter().filter(|i| i.ended_at.is_none()).count());

    Ok(IncidentsData {
        last_status,
        last_at_iso,
        rows: incidents.into_iter().map(IncidentRow::from).collect(),
        has_more,
        ongoing_count,
    })
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
    let data = load_incidents_data(&state, org, target.id, time_range).await?;
    let (kind, address) = describe_check(&target.check);
    let share_count = state
        .monitor_share_store
        .count_active_for_target(org, target.id)
        .await? as usize;

    Ok(IncidentsPage {
        active_tab: "targets",
        subtab: SUBTAB_INCIDENTS,
        ongoing_count: data.ongoing_count,
        id: target.id.to_string(),
        name: target.name,
        kind,
        address,
        interval_s: target.interval.as_secs(),
        enabled: target.enabled,
        tags: target.tags,
        managed_by: target.write_source.managed_label(),
        share_count,
        last_status: data.last_status,
        last_at_iso: data.last_at_iso,
        incidents: data.rows,
        incidents_has_more: data.has_more,
        results_base: format!("/api/v1/targets/{}", target.id),
        range: range_key,
        range_options: build_range_options(range_key, &INCIDENT_RANGE_KEYS),
        range_base_path: format!("/targets/{}/incidents", target.id),
        from_iso: labels.from_iso,
        to_iso: labels.to_iso,
        from_human: labels.from_human,
        to_human: labels.to_human,
        selected_region: None,
    })
}

pub(crate) fn resolve_incident_window(
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

    #[test]
    fn region_breakdown_greys_dead_region_keeps_live() {
        let rollup = |region: &str, last: &str| crate::api::types::RegionRollup {
            region: region.into(),
            samples: 10,
            up: 10,
            p50_ms: 100,
            p95_ms: 200,
            last_status: last.into(),
        };
        let live: std::collections::HashSet<String> = ["eu-west".to_string()].into_iter().collect();

        let alive =
            RegionBreakdownRow::from_rollup(rollup("eu-west", "up"), None, &[], Some(&live));
        assert_eq!(alive.last_status, "up");

        let dead = RegionBreakdownRow::from_rollup(rollup("apac-sg", "up"), None, &[], Some(&live));
        assert_eq!(dead.last_status, "no_data");

        // Liveness unknown (query failed) leaves the status untouched.
        let unknown = RegionBreakdownRow::from_rollup(rollup("apac-sg", "up"), None, &[], None);
        assert_eq!(unknown.last_status, "up");
    }

    fn sample_page() -> DetailPage {
        DetailPage {
            active_tab: "targets",
            subtab: SUBTAB_MONITOR,
            ongoing_count: 0,
            id: "00000000-0000-0000-0000-000000000001".into(),
            name: "api".into(),
            kind: "HTTP",
            address: "https://example.com".into(),
            registered_domain: None,
            interval_s: 60,
            enabled: true,
            tags: vec!["prod".into()],
            managed_by: None,
            share_count: 0,
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
            kpi: Arc::new(KpiTrend::default()),
            segments: Arc::from(vec![
                StatusSeg {
                    class: "op",
                    time: "12:00".into(),
                    stat: "100.0%".into(),
                    from_iso: "2026-05-13T12:00:00Z".into(),
                    to_iso: "2026-05-13T12:30:00Z".into(),
                    total: 60,
                    bad: 0,
                },
                StatusSeg {
                    class: "maj",
                    time: "12:30".into(),
                    stat: "0.0%".into(),
                    from_iso: "2026-05-13T12:30:00Z".into(),
                    to_iso: "2026-05-13T13:00:00Z".into(),
                    total: 60,
                    bad: 60,
                },
            ]),
            ribbon_oob: false,
            config_json: r#"{"type":"http"}"#.into(),
            range: "24h",
            range_options: build_range_options("24h", &RANGE_KEYS),
            range_base_path: "/targets/00000000-0000-0000-0000-000000000001".into(),
            from_iso: "2026-05-12T12:00:00Z".into(),
            to_iso: "2026-05-13T12:00:00Z".into(),
            from_human: "2026-05-12 12:00 UTC".into(),
            to_human: "2026-05-13 12:00 UTC".into(),
            regions: Vec::new(),
            selected_region: None,
            region_breakdown: Vec::new(),
            heartbeat: None,
        }
    }

    #[test]
    fn heartbeat_detail_renders_ping_card_without_probe_surfaces() {
        let mut p = sample_page();
        p.kind = "HEARTBEAT";
        p.heartbeat = Some(crate::api::handlers::targets::HeartbeatInfo {
            ping_url: Some("https://app.example.com/ping/tok123".into()),
            last_ping_at: None,
        });
        let html = p.render().unwrap();
        assert!(html.contains("https://app.example.com/ping/tok123"));
        assert!(html.contains(r##"data-copy="#hb-ping-url""##));
        assert!(html.contains("last ping:"));
        assert!(html.contains("never"));
        // No probe surfaces for a passive kind.
        assert!(!html.contains("data-detail-test-now"));
        assert!(!html.contains("latency (p50/p95/p99)"));
    }

    #[test]
    fn status_segments_map_availability_to_ribbon_classes() {
        let base = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let range = ClampedRange::unclamped(TimeRange {
            from: base,
            to: base + Duration::hours(1),
        });
        // 900s buckets → four cells; classes follow the dashboard thresholds.
        let counts = [(100, 100), (100, 97), (100, 80), (0, 0)];
        let segs = status_segments(&counts, range, 900);
        let classes: Vec<&str> = segs.iter().map(|s| s.class).collect();
        assert_eq!(classes, ["op", "deg", "maj", "none"]);
        assert_eq!(segs[0].stat, "100.0%");
        assert_eq!(segs[3].stat, "no data");
        // Counts carry through for the drawer's scale line.
        assert_eq!((segs[2].total, segs[2].bad), (100, 20));
        assert_eq!((segs[3].total, segs[3].bad), (0, 0));
        // Each cell carries a non-empty, contiguous window for the drill drawer.
        assert!(!segs[0].from_iso.is_empty() && !segs[0].to_iso.is_empty());
        assert!(segs[0].from_iso < segs[0].to_iso);
        assert_eq!(segs[0].to_iso, segs[1].from_iso, "buckets are contiguous");
    }

    #[test]
    fn drawer_check_rows_render_region_column_and_expand() {
        let mut r = CheckResult {
            target_id: Uuid::nil(),
            org_id: Uuid::nil(),
            timestamp: "2026-05-13T12:00:00Z".parse().unwrap(),
            status: crate::domain::CheckStatus::Error,
            duration_ms: 1204,
            dns_ms: Some(12),
            connect_ms: Some(40),
            tls_ms: None,
            ttfb_ms: None,
            response_code: Some(503),
            response_size: None,
            error: Some("connection refused".into()),
        };
        let rows: Arc<[ResultRow]> =
            Arc::from(vec![ResultRow::with_region("apac-sg".into(), r.clone())]);
        let html = DetailCheckRows {
            results: rows,
            show_region: true,
        }
        .render()
        .unwrap();
        // Region cell present; failing row is expandable with a timing detail row.
        assert!(html.contains("apac-sg"));
        assert!(html.contains("data-result-row"));
        assert!(html.contains("data-result-detail"));

        // Region-agnostic table (recent results) hides the column.
        r.error = None;
        let plain: Arc<[ResultRow]> = Arc::from(vec![ResultRow::from(r)]);
        let plain_html = DetailCheckRows {
            results: plain,
            show_region: false,
        }
        .render()
        .unwrap();
        assert!(!plain_html.contains("apac-sg"));
    }

    #[test]
    fn detail_renders_status_ribbon() {
        let html = sample_page().render().unwrap();
        assert!(html.contains("dashboard-ribbon"));
        assert!(html.contains("dashboard-ribbon__seg--op"));
        assert!(html.contains("dashboard-ribbon__seg--maj"));
        assert!(html.contains(r#"data-tip-stat="100.0%""#));
        // Failing cell is a drill button carrying its window; healthy cell is not.
        assert!(html.contains("data-ribbon-drill"));
        assert!(html.contains(r#"data-from="2026-05-13T12:30:00Z""#));
    }

    #[test]
    fn detail_renders_header_and_widgets() {
        let html = sample_page().render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("api"));
        assert!(html.contains("Uptime"));
        assert!(html.contains("99.00"));
        assert!(html.contains("data-endpoint"));
        // Charts read the server-bucketed latency endpoint; recent results are
        // a server-rendered table, not an API fetch.
        assert!(html.contains("/api/v1/targets/00000000-0000-0000-0000-000000000001/latency"));
    }

    #[test]
    fn detail_header_folds_secondary_actions_into_overflow_menu() {
        let html = sample_page().render().unwrap();
        // Primary actions stay visible.
        assert!(html.contains("run check now"));
        assert!(html.contains("data-share-open"));
        // Secondary actions live inside the ⋯ overflow menu.
        assert!(html.contains("hdr-menu__panel"));
        assert!(html.contains(r#"class="hdr-menu__item""#));
        assert!(html.contains("hdr-menu__item--danger"));
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
    fn detail_header_shows_shared_chip_only_when_links_exist() {
        // No links → no chip.
        assert!(!sample_page().render().unwrap().contains("[ shared:"));
        // Live links → a chip that opens the share modal.
        let mut p = sample_page();
        p.share_count = 2;
        let html = p.render().unwrap();
        assert!(html.contains("[ shared:2 ]"));
        assert!(html.contains("data-share-open"));
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
            enabled: true,
            last_status: "up",
            uptime: Arc::new(UptimeStatsView {
                total: 100,
                up: 99,
                down: 1,
                degraded: 0,
                error: 0,
                uptime_pct: "99.00".into(),
            }),
            kpi: Arc::new(KpiTrend::default()),
            last_at_iso: Arc::from("2026-05-13T12:00:00Z"),
            selected_region: None,
            segments: Arc::from(Vec::<StatusSeg>::new()),
            ribbon_oob: true,
        }
    }

    #[test]
    fn live_partial_renders_kpi_swap_target_plus_oob_ribbon() {
        let html = sample_live().render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(html.contains(r#"id="detail-live-kpi""#));
        assert!(html.contains(
            "hx-get=\"/web/partials/targets/00000000-0000-0000-0000-000000000001/live?range=24h\""
        ));
        assert!(html.contains(r#"hx-trigger="every 60s, sm:refresh-live from:body""#));
        assert!(html.contains(r#"hx-swap="outerHTML""#));
        assert!(html.contains(r#"data-newest-ts="2026-05-13T12:00:00Z""#));
        // The ribbon rides along as an out-of-band swap so the newest cell stays
        // current without a full-page reload; the recent-results table is gone.
        assert!(html.contains(r#"id="detail-ribbon""#));
        assert!(html.contains(r#"hx-swap-oob="true""#));
        assert!(!html.contains(r#"id="detail-live-recent""#));
        assert!(html.contains("99.00"));
    }

    #[test]
    fn detail_page_orders_kpi_charts_ribbon() {
        let html = sample_page().render().unwrap();
        assert!(html.contains(r#"id="detail-ribbon""#));
        assert!(html.contains(r#"id="detail-live-kpi""#));
        assert!(html.contains(r#"id="latency-chart""#));
        // Recent-results table replaced by the ribbon + drill drawer, which now
        // sits below the charts (and the by-region table).
        assert!(!html.contains(r#"id="detail-live-recent""#));
        let kpi_pos = html.find(r#"id="detail-live-kpi""#).expect("kpi present");
        let chart_pos = html.find(r#"id="latency-chart""#).expect("chart present");
        let ribbon_pos = html.find(r#"id="detail-ribbon""#).expect("ribbon present");
        assert!(kpi_pos < chart_pos, "KPI renders before charts");
        assert!(chart_pos < ribbon_pos, "ribbon renders after charts");
    }

    fn kpi_ranges() -> (ClampedRange, ClampedRange) {
        let base = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let range = ClampedRange::unclamped(TimeRange {
            from: base,
            to: base + Duration::hours(1),
        });
        let prior = ClampedRange::unclamped(TimeRange {
            from: base - Duration::hours(1),
            to: base,
        });
        (range, prior)
    }

    fn stats(total: u64, up: u64, down: u64, error: u64, pct: f64) -> UptimeStats {
        UptimeStats {
            total,
            up,
            down,
            degraded: 0,
            error,
            uptime_pct: pct,
        }
    }

    fn trend(
        current: &UptimeStats,
        prior: &UptimeStats,
        avail: &[AvailabilityBucket],
        range: ClampedRange,
        prior_range: ClampedRange,
        confirmed: bool,
    ) -> KpiTrend {
        build_kpi_trend(KpiInputs {
            current,
            prior,
            cur_incidents: &[],
            prior_incidents: &[],
            avail,
            range,
            prior_range,
            spark_bucket_seconds: 60,
            confirmed,
        })
    }

    fn one_bucket(range: ClampedRange, total: u64, up: u64) -> [AvailabilityBucket; 1] {
        [AvailabilityBucket {
            bucket_ts: range.from.timestamp(),
            total,
            up,
        }]
    }

    #[test]
    fn build_kpi_trend_deltas_vs_prior() {
        let (range, prior_range) = kpi_ranges();
        let avail = one_bucket(range, 100, 99);
        let kpi = trend(
            &stats(100, 99, 1, 0, 99.0),
            &stats(100, 95, 3, 2, 95.0),
            &avail,
            range,
            prior_range,
            false,
        );
        assert!(!kpi.spark_path.is_empty());
        assert_eq!(kpi.uptime_delta.unwrap().body, "+4.00 pp");
        assert_eq!(kpi.up_delta.unwrap().body, "+4");
        assert_eq!(kpi.down_delta.unwrap().body, "-2");
        assert_eq!(kpi.error_delta.unwrap().body, "-2");
    }

    #[test]
    fn build_kpi_trend_no_prior_keeps_spark_drops_deltas() {
        let (range, prior_range) = kpi_ranges();
        let avail = one_bucket(range, 100, 99);
        let kpi = trend(
            &stats(100, 99, 1, 0, 99.0),
            &stats(0, 0, 0, 0, 0.0),
            &avail,
            range,
            prior_range,
            false,
        );
        assert!(!kpi.spark_path.is_empty());
        assert!(kpi.uptime_delta.is_none());
        assert!(kpi.up_delta.is_none());
    }

    #[test]
    fn build_kpi_trend_empty_current_drops_deltas() {
        let (range, prior_range) = kpi_ranges();
        let avail = one_bucket(range, 0, 0);
        // No samples this window: an empty current must not read as -100pp.
        let kpi = trend(
            &stats(0, 0, 0, 0, 0.0),
            &stats(100, 95, 3, 2, 95.0),
            &avail,
            range,
            prior_range,
            false,
        );
        assert!(kpi.uptime_delta.is_none());
        assert!(kpi.up_delta.is_none());
    }

    #[test]
    fn build_kpi_trend_truncated_prior_drops_deltas() {
        let base = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let range = ClampedRange::unclamped(TimeRange {
            from: base,
            to: base + Duration::hours(1),
        });
        // Prior shortened by the retention floor (30m vs the 1h current).
        let prior_range = ClampedRange::unclamped(TimeRange {
            from: base - Duration::minutes(30),
            to: base,
        });
        let avail = one_bucket(range, 100, 99);
        let kpi = trend(
            &stats(100, 99, 1, 0, 99.0),
            &stats(50, 48, 1, 1, 96.0),
            &avail,
            range,
            prior_range,
            false,
        );
        assert!(!kpi.spark_path.is_empty());
        assert!(
            kpi.up_delta.is_none(),
            "skewed unequal-window counts dropped"
        );
    }

    #[test]
    fn confirmed_spark_decomposes_headline_not_raw_ratio() {
        let (range, prior_range) = kpi_ranges();
        // Raw bucket reads 50% up, but no confirmed incident overlaps it.
        let avail = one_bucket(range, 100, 50);
        let current = stats(100, 50, 50, 0, 50.0);
        let prior = stats(100, 100, 0, 0, 100.0);
        let confirmed = trend(&current, &prior, &avail, range, prior_range, true);
        let raw = trend(&current, &prior, &avail, range, prior_range, false);
        assert_ne!(confirmed.spark_path, raw.spark_path);
        // Confirmed: flat at the top (no incident → 100%). Raw: mid (50%).
        assert!(
            confirmed.spark_path.ends_with(" 0.0"),
            "confirmed flat-top: {}",
            confirmed.spark_path
        );
        assert!(
            raw.spark_path.ends_with(" 11.0"),
            "raw 50% mid: {}",
            raw.spark_path
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
            regions_down: Vec::new(),
            regions_up: Vec::new(),
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
            managed_by: None,
            share_count: 0,
            last_status: "down",
            last_at_iso: "2026-05-13T12:00:00Z".into(),
            incidents,
            incidents_has_more: false,
            results_base: "/api/v1/targets/00000000-0000-0000-0000-000000000001".into(),
            range: "30d",
            range_options: build_range_options("30d", &INCIDENT_RANGE_KEYS),
            range_base_path: "/targets/00000000-0000-0000-0000-000000000001/incidents".into(),
            from_iso: "2026-04-13T12:00:00Z".into(),
            to_iso: "2026-05-13T12:00:00Z".into(),
            from_human: "2026-04-13 12:00 UTC".into(),
            to_human: "2026-05-13 12:00 UTC".into(),
            selected_region: None,
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
        assert!(html.contains("no incidents in the last 30d"));
        assert!(!html.contains("<table"));
        assert!(html.contains("aria-current=\"page\""));
    }

    #[test]
    fn incidents_page_renders_table_rows_with_ongoing_emphasis() {
        let html = sample_incidents_page(vec![ongoing_row(), resolved_row()], 1)
            .render()
            .unwrap();
        assert!(html.contains("<table"));
        // Ongoing emphasis: themed left border + pulsing badge + severity-tagged label.
        assert!(html.contains("sm-incident-ongoing"));
        assert!(html.contains("animate-pulse"));
        assert!(html.contains("ongoing · down"));
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
