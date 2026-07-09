//! Free standalone developer tools served from the apex host. Each is a
//! self-contained, no-DB page that ranks for an adjacent query and routes
//! the visitor into the product. Same cached-render contract as the rest
//! of marketing: one render at boot, ETag + Cache-Control on every hit.

use std::sync::Arc;
use std::sync::OnceLock;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::Response;

use crate::marketing::seo::{
    JsonLd, OpenGraph, json_ld_breadcrumb, json_ld_faqpage, json_ld_web_application,
    json_ld_webpage,
};
use crate::web::filters;

use super::config::{BRAND, MarketingCfg};
use super::pages::{CachedRender, cached_render, serve_cached};

const TOOL_CACHE_CONTROL: HeaderValue =
    HeaderValue::from_static("public, max-age=300, stale-while-revalidate=86400");

pub const UPTIME_SLA_PATH: &str = "/tools/uptime-sla-calculator";
const UPTIME_SLA_CREATED: &str = "2026-07-09";
const UPTIME_SLA_LASTMOD: &str = "2026-07-09";
pub const UPTIME_SLA_TITLE: &str = "Uptime SLA & Downtime Calculator";
pub const UPTIME_SLA_DESCRIPTION: &str = "Turn an uptime percentage into allowed downtime per day, week, month and year. A free SLA and SLO calculator with the full nines reference table.";

const DAY: f64 = 86_400.0;
const WEEK: f64 = 604_800.0;
const MONTH: f64 = 2_592_000.0; // 30 days
const YEAR: f64 = 31_536_000.0; // 365 days
const CHECK_INTERVAL: f64 = 60.0;

/// Uptime targets shown in the server-rendered reference table.
const REFERENCE_NINES: &[f64] = &[90.0, 95.0, 99.0, 99.5, 99.9, 99.95, 99.99, 99.999];
/// One-click targets on the interactive widget.
const PRESETS: &[f64] = &[99.0, 99.5, 99.9, 99.95, 99.99, 99.999];
/// The state the page renders with before any JS runs.
const DEFAULT_UPTIME: f64 = 99.9;

const UPTIME_SLA_FAQS: &[(&str, &str)] = &[
    (
        "How do you calculate allowed downtime from an uptime percentage?",
        "Multiply the length of the period by one minus the uptime fraction. \
         At 99.9%, a 30-day month allows 30 days times 0.001, which is 43 minutes \
         and 12 seconds of downtime.",
    ),
    (
        "How do I calculate an uptime percentage?",
        "Subtract the downtime share from 100%. If a service was down 43 minutes \
         in a 30-day month, uptime is (2,592,000 seconds minus 2,580) divided by \
         2,592,000, which is about 99.9%. The reverse calculator above turns a \
         downtime figure straight into the percentage.",
    ),
    (
        "What is the difference between an SLA and an SLO?",
        "An SLO is the reliability target you set internally, such as 99.9% uptime. \
         An SLA is the contract you sign with a customer, usually a looser number \
         with a refund if you miss it. Both use the same downtime math.",
    ),
    (
        "How much downtime does 99.9% allow?",
        "Three nines allows about 8 hours 45 minutes a year, 43 minutes a month, \
         or 1 minute 26 seconds a day. 99.99% cuts that to roughly 52 minutes a year.",
    ),
    (
        "What uptime should I aim for?",
        "Most SaaS products commit to 99.9% and run internally toward 99.95%. Chasing \
         a fifth nine is expensive and rarely worth it unless a payment or safety flow \
         depends on it. Pick a number you can actually measure and alert on.",
    ),
    (
        "How is the month and year length defined here?",
        "A month is 30 days and a year is 365 days, the convention most SLA tables use. \
         Day and week are exact. Change the percentage and every column recomputes.",
    ),
];

/// One reference-table row: an uptime target and its allowed downtime per period.
pub struct SlaRow {
    pub label: String,
    pub daily: String,
    pub weekly: String,
    pub monthly: String,
    pub yearly: String,
}

/// The interactive widget's default state, rendered server-side so the page
/// shows real numbers with JS disabled.
pub struct SlaResult {
    pub daily: String,
    pub weekly: String,
    pub monthly: String,
    pub yearly: String,
    pub checks_monthly: String,
}

/// Reverse widget default: a downtime figure resolved back to an uptime %.
const REVERSE_VALUE: &str = "1";
const REVERSE_UNIT_SECS: f64 = 3_600.0; // hour
const REVERSE_PERIOD_SECS: f64 = MONTH;

#[derive(Template, WebTemplate)]
#[template(path = "marketing/tool_uptime_sla.html")]
struct UptimeSlaPage {
    app_url: String,
    canonical_url: String,
    og: OpenGraph,
    breadcrumb_json_ld: JsonLd,
    web_application_json_ld: JsonLd,
    webpage_json_ld: JsonLd,
    faq_json_ld: JsonLd,
    faqs: &'static [(&'static str, &'static str)],
    rows: Vec<SlaRow>,
    default: SlaResult,
    default_uptime: String,
    presets: &'static [f64],
    reverse_value: &'static str,
    reverse_uptime: String,
    version: &'static str,
}

/// Allowed downtime for an uptime percentage over a period, in seconds.
fn downtime(uptime_pct: f64, period_secs: f64) -> f64 {
    period_secs * (1.0 - uptime_pct / 100.0)
}

/// Human-readable duration. Sub-second falls to `ms`, under ten seconds keeps
/// one decimal, larger values break into `d/h/m/s`. Kept in lockstep with the
/// JavaScript port so the live widget never disagrees with the server render.
fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0s".to_string();
    }
    if secs < 1.0 {
        return format!("{}ms", (secs * 1000.0).round());
    }
    if secs < 10.0 {
        let v = (secs * 10.0).round() / 10.0;
        if v.fract() == 0.0 {
            return format!("{v}s");
        }
        return format!("{v:.1}s");
    }
    let mut total = secs.round() as u64;
    let d = total / 86_400;
    total %= 86_400;
    let h = total / 3_600;
    total %= 3_600;
    let m = total / 60;
    let s = total % 60;
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if s > 0 {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
}

fn sla_result(uptime: f64) -> SlaResult {
    SlaResult {
        daily: human_duration(downtime(uptime, DAY)),
        weekly: human_duration(downtime(uptime, WEEK)),
        monthly: human_duration(downtime(uptime, MONTH)),
        yearly: human_duration(downtime(uptime, YEAR)),
        checks_monthly: format!(
            "{}",
            (downtime(uptime, MONTH) / CHECK_INTERVAL).round() as u64
        ),
    }
}

/// Uptime percentage for a downtime figure over a period. Inverse of
/// [`downtime`]; clamped at zero so an over-budget entry reads 0%.
fn uptime_for(downtime_secs: f64, period_secs: f64) -> f64 {
    (1.0 - downtime_secs / period_secs).max(0.0) * 100.0
}

/// Format a percentage with up to four decimals, trailing zeros trimmed.
fn format_pct(pct: f64) -> String {
    let s = format!("{pct:.4}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}%")
}

fn reference_rows() -> Vec<SlaRow> {
    REFERENCE_NINES
        .iter()
        .map(|&pct| SlaRow {
            label: format!("{pct}%"),
            daily: human_duration(downtime(pct, DAY)),
            weekly: human_duration(downtime(pct, WEEK)),
            monthly: human_duration(downtime(pct, MONTH)),
            yearly: human_duration(downtime(pct, YEAR)),
        })
        .collect()
}

static UPTIME_SLA_CACHED: OnceLock<CachedRender> = OnceLock::new();

fn render_uptime_sla(cfg: &MarketingCfg) -> CachedRender {
    let canonical_url = format!("{}{}", cfg.canonical_origin, UPTIME_SLA_PATH);
    let mut og = OpenGraph::default_for(&format!("{UPTIME_SLA_TITLE} | {BRAND}"), &canonical_url);
    og.description = UPTIME_SLA_DESCRIPTION.to_string();
    let page = UptimeSlaPage {
        app_url: cfg.app_url.clone(),
        breadcrumb_json_ld: json_ld_breadcrumb(
            &cfg.canonical_origin,
            UPTIME_SLA_TITLE,
            UPTIME_SLA_PATH,
        ),
        web_application_json_ld: json_ld_web_application(
            &cfg.canonical_origin,
            UPTIME_SLA_TITLE,
            UPTIME_SLA_PATH,
            UPTIME_SLA_DESCRIPTION,
        ),
        webpage_json_ld: json_ld_webpage(
            &cfg.canonical_origin,
            UPTIME_SLA_PATH,
            UPTIME_SLA_TITLE,
            UPTIME_SLA_CREATED,
            UPTIME_SLA_LASTMOD,
        ),
        faq_json_ld: json_ld_faqpage(UPTIME_SLA_FAQS),
        faqs: UPTIME_SLA_FAQS,
        rows: reference_rows(),
        default: sla_result(DEFAULT_UPTIME),
        default_uptime: format!("{DEFAULT_UPTIME}"),
        presets: PRESETS,
        reverse_value: REVERSE_VALUE,
        reverse_uptime: format_pct(uptime_for(REVERSE_UNIT_SECS, REVERSE_PERIOD_SECS)),
        canonical_url,
        og,
        version: env!("CARGO_PKG_VERSION"),
    };
    let body = page
        .render()
        .unwrap_or_else(|e| format!("<!-- uptime-sla render failed: {e} -->"));
    cached_render(body)
}

async fn uptime_sla(State(cfg): State<Arc<MarketingCfg>>, headers: HeaderMap) -> Response {
    let cached = UPTIME_SLA_CACHED.get_or_init(|| render_uptime_sla(&cfg));
    serve_cached(&headers, cached, &TOOL_CACHE_CONTROL)
}

/// Router entries for every tool. Single source shared with sitemap + llms.
pub fn mount(router: axum::Router<Arc<MarketingCfg>>) -> axum::Router<Arc<MarketingCfg>> {
    router.route(UPTIME_SLA_PATH, axum::routing::get(uptime_sla))
}

pub(crate) fn warm(cfg: &MarketingCfg) {
    UPTIME_SLA_CACHED.get_or_init(|| render_uptime_sla(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downtime_matches_known_slas() {
        // 99.9% over a 30-day month is 43m 12s.
        assert_eq!(human_duration(downtime(99.9, MONTH)), "43m 12s");
        // 99.99% over a year is 52m 34s.
        assert_eq!(human_duration(downtime(99.99, YEAR)), "52m 34s");
        // Five nines over a day is sub-second.
        assert_eq!(human_duration(downtime(99.999, DAY)), "864ms");
    }

    #[test]
    fn reference_table_is_complete() {
        let rows = reference_rows();
        assert_eq!(rows.len(), REFERENCE_NINES.len());
        assert_eq!(rows[0].label, "90%");
        assert!(rows.iter().all(|r| !r.yearly.is_empty()));
    }

    #[test]
    fn reverse_resolves_downtime_to_uptime() {
        // One hour of downtime in a 30-day month is ~99.8611%.
        assert_eq!(format_pct(uptime_for(3_600.0, MONTH)), "99.8611%");
        // A clean 0.1% budget round-trips to 99.9%, no trailing zeros.
        assert_eq!(format_pct(uptime_for(downtime(99.9, YEAR), YEAR)), "99.9%");
        // Over-budget clamps at zero, never negative.
        assert_eq!(format_pct(uptime_for(MONTH * 2.0, MONTH)), "0%");
    }

    #[test]
    fn default_result_ties_to_check_interval() {
        let r = sla_result(99.9);
        assert_eq!(r.monthly, "43m 12s");
        // 2592s of monthly downtime is ~43 missed 60s checks.
        assert_eq!(r.checks_monthly, "43");
    }
}
