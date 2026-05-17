use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::CheckResult;
use crate::error::AppError;
use crate::storage::{TimeRange, UptimeStats};
use crate::web::AuthedBrowser;
use crate::web::assets::filters;
use crate::web::error::WebResult;
use crate::web::views::{describe_check, fmt_ts};

const RESULTS_PAGE_LIMIT: usize = 200;
const RANGE_KEYS: [&str; 4] = ["1h", "24h", "7d", "30d"];
const DEFAULT_RANGE: &str = "24h";

#[derive(Debug, Default, Deserialize)]
pub struct DetailParams {
    #[serde(default)]
    pub range: Option<String>,
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
}

pub struct ResultRow {
    pub timestamp: String,
    pub status: &'static str,
    pub duration_ms: u32,
    pub response_code: String,
    pub error: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/detail.html")]
pub struct DetailPage {
    pub active_tab: &'static str,
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub address: String,
    pub interval_s: u64,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub last_status: &'static str,
    pub uptime: UptimeStatsView,
    pub results: Vec<ResultRow>,
    pub results_total: u64,
    pub config_json: String,
    pub range: &'static str,
    pub range_options: Vec<RangeOption>,
    pub from_iso: String,
    pub to_iso: String,
}

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

pub struct RangeOption {
    pub key: &'static str,
    pub selected: bool,
}

pub async fn index(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<DetailParams>,
) -> WebResult<DetailPage> {
    let target = state
        .target_store
        .get(id)
        .await?
        .ok_or_else(|| AppError::not_found("TARGET_NOT_FOUND", "target not found"))?;

    let range_key = resolve_range_key(params.range.as_deref());
    let (from, to) = resolve_window(range_key, params.from, params.to);
    let time_range = TimeRange { from, to };

    let (uptime, results, results_total) = tokio::try_join!(
        state.results_store.uptime(target.id, time_range),
        state
            .results_store
            .list_results(target.id, time_range, RESULTS_PAGE_LIMIT, 0),
        state.results_store.count_results(target.id, time_range),
    )?;

    let last_status = results.first().map(|r| r.status.as_str()).unwrap_or("");
    let result_rows = results.into_iter().map(ResultRow::from).collect();
    let config_json = serde_json::to_string_pretty(&target.check)
        .map_err(|e| AppError::Other(anyhow::anyhow!(e)))?;
    let (kind, address) = describe_check(&target.check);

    Ok(DetailPage {
        active_tab: "targets",
        id: target.id.to_string(),
        name: target.name,
        kind,
        address,
        interval_s: target.interval.as_secs(),
        enabled: target.enabled,
        tags: target.tags,
        last_status,
        uptime: uptime.into(),
        results: result_rows,
        results_total,
        config_json,
        range: range_key,
        range_options: build_range_options(range_key),
        from_iso: fmt_ts(from),
        to_iso: fmt_ts(to),
    })
}

fn resolve_range_key(raw: Option<&str>) -> &'static str {
    raw.and_then(|s| RANGE_KEYS.iter().copied().find(|k| *k == s))
        .unwrap_or(DEFAULT_RANGE)
}

/// Maps a preset range key to a window. Explicit `from`/`to` query params
/// override the preset (custom range case).
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
        _ => Duration::hours(24),
    };
    (from.unwrap_or(to - span), to)
}

fn build_range_options(active: &'static str) -> Vec<RangeOption> {
    RANGE_KEYS
        .iter()
        .map(|k| RangeOption {
            key: k,
            selected: *k == active,
        })
        .collect()
}

impl From<CheckResult> for ResultRow {
    fn from(r: CheckResult) -> Self {
        Self {
            timestamp: fmt_ts(r.timestamp),
            status: r.status.as_str(),
            duration_ms: r.duration_ms,
            response_code: r.response_code.map(|c| c.to_string()).unwrap_or_default(),
            error: r.error.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_page() -> DetailPage {
        DetailPage {
            active_tab: "targets",
            id: "00000000-0000-0000-0000-000000000001".into(),
            name: "api".into(),
            kind: "HTTP",
            address: "https://example.com".into(),
            interval_s: 60,
            enabled: true,
            tags: vec!["prod".into()],
            last_status: "up",
            uptime: UptimeStatsView {
                total: 100,
                up: 99,
                down: 1,
                degraded: 0,
                error: 0,
                uptime_pct: "99.00".into(),
            },
            results: vec![ResultRow {
                timestamp: "2026-05-13T12:00:00Z".into(),
                status: "up",
                duration_ms: 42,
                response_code: "200".into(),
                error: String::new(),
            }],
            results_total: 1,
            config_json: r#"{"type":"http"}"#.into(),
            range: "24h",
            range_options: build_range_options("24h"),
            from_iso: "2026-05-12T12:00:00Z".into(),
            to_iso: "2026-05-13T12:00:00Z".into(),
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
    fn range_options_mark_active() {
        let opts = build_range_options("7d");
        assert!(opts.iter().any(|o| o.key == "7d" && o.selected));
        assert_eq!(opts.iter().filter(|o| o.selected).count(), 1);
    }

    #[test]
    fn resolve_range_key_clamps_to_allowed() {
        assert_eq!(resolve_range_key(Some("1h")), "1h");
        assert_eq!(resolve_range_key(Some("garbage")), "24h");
        assert_eq!(resolve_range_key(None), "24h");
    }
}
