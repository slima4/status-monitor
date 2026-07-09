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
    let mut og = OpenGraph::default_for(
        &format!("{UPTIME_SLA_TITLE} | {BRAND}"),
        &canonical_url,
        &cfg.canonical_origin,
    );
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

// ── Cron expression generator ─────────────────────────────────────────
// Client-side only (cron.js). The server default's authored description
// must stay identical to the JS output, or it flickers on hydration.

pub const CRON_PATH: &str = "/tools/cron-expression-generator";
const CRON_CREATED: &str = "2026-07-09";
const CRON_LASTMOD: &str = "2026-07-09";
pub const CRON_TITLE: &str = "Cron Expression Generator & Parser";
pub const CRON_DESCRIPTION: &str = "Build and read cron expressions in plain English, with the next run times and a reference table of the most common schedules. Free, no sign-up.";
const CRON_DEFAULT_EXPR: &str = "*/15 9-17 * * 1-5";
const CRON_DEFAULT_DESC: &str =
    "At every 15th minute past every hour from 9 through 17, on Monday through Friday.";

/// Common schedules for the reference table (expression, plain-English).
const CRON_EXAMPLES: &[(&str, &str)] = &[
    ("* * * * *", "Every minute"),
    ("*/5 * * * *", "Every 5 minutes"),
    ("0 * * * *", "Every hour, on the hour"),
    (
        "*/15 9-17 * * 1-5",
        "Every 15 minutes, 9am to 5pm, on weekdays",
    ),
    ("0 0 * * *", "Every day at midnight"),
    ("30 2 * * *", "Every day at 02:30"),
    ("0 9 * * 1-5", "At 09:00, Monday to Friday"),
    ("0 0 * * 0", "Every Sunday at midnight"),
    ("0 0 1 * *", "Midnight on the 1st of each month"),
    ("0 0 1 1 *", "Midnight on the 1st of January"),
];

/// One-click schedules on the interactive widget (expression, label).
const CRON_PRESETS: &[(&str, &str)] = &[
    ("*/5 * * * *", "every 5 min"),
    ("0 * * * *", "hourly"),
    ("0 0 * * *", "daily"),
    ("0 9 * * 1-5", "weekdays 9am"),
    ("0 0 * * 0", "weekly"),
    ("0 0 1 * *", "monthly"),
];

const CRON_FAQS: &[(&str, &str)] = &[
    (
        "What is a cron expression?",
        "Five fields that tell a scheduler when to run a job: minute, hour, \
         day of the month, month, and day of the week. <code class=\"mk-chip\" \
         translate=\"no\">0 9 * * 1-5</code> means 9am on weekdays.",
    ),
    (
        "What does the slash mean, like */15?",
        "A step. <code class=\"mk-chip\" translate=\"no\">*/15</code> in the minute \
         field means every 15th minute (0, 15, 30, 45). <code class=\"mk-chip\" \
         translate=\"no\">*/2</code> in the hour field means every other hour.",
    ),
    (
        "Are the next run times UTC or local?",
        "The times shown here are in your browser's timezone. On a server, cron \
         uses that machine's timezone, which is usually UTC. Check the box's \
         timezone before you trust a schedule in production.",
    ),
    (
        "What happens when both day fields are set?",
        "If you restrict both the day of the month and the day of the week, cron \
         runs when EITHER matches, not both. So <code class=\"mk-chip\" \
         translate=\"no\">0 0 1 * 1</code> fires on the 1st and every Monday.",
    ),
    (
        "Do shortcuts like @daily work?",
        "Yes. The parser accepts @hourly, @daily, @weekly, @monthly and @yearly \
         and expands them to the equivalent five-field expression.",
    ),
];

#[derive(Template, WebTemplate)]
#[template(path = "marketing/tool_cron.html")]
struct CronPage {
    app_url: String,
    canonical_url: String,
    og: OpenGraph,
    breadcrumb_json_ld: JsonLd,
    web_application_json_ld: JsonLd,
    webpage_json_ld: JsonLd,
    faq_json_ld: JsonLd,
    faqs: &'static [(&'static str, &'static str)],
    examples: &'static [(&'static str, &'static str)],
    presets: &'static [(&'static str, &'static str)],
    default_expr: &'static str,
    default_desc: &'static str,
    version: &'static str,
}

static CRON_CACHED: OnceLock<CachedRender> = OnceLock::new();

fn render_cron(cfg: &MarketingCfg) -> CachedRender {
    let canonical_url = format!("{}{}", cfg.canonical_origin, CRON_PATH);
    let mut og = OpenGraph::default_for(
        &format!("{CRON_TITLE} | {BRAND}"),
        &canonical_url,
        &cfg.canonical_origin,
    );
    og.description = CRON_DESCRIPTION.to_string();
    let page = CronPage {
        app_url: cfg.app_url.clone(),
        breadcrumb_json_ld: json_ld_breadcrumb(&cfg.canonical_origin, CRON_TITLE, CRON_PATH),
        web_application_json_ld: json_ld_web_application(
            &cfg.canonical_origin,
            CRON_TITLE,
            CRON_PATH,
            CRON_DESCRIPTION,
        ),
        webpage_json_ld: json_ld_webpage(
            &cfg.canonical_origin,
            CRON_PATH,
            CRON_TITLE,
            CRON_CREATED,
            CRON_LASTMOD,
        ),
        faq_json_ld: json_ld_faqpage(CRON_FAQS),
        faqs: CRON_FAQS,
        examples: CRON_EXAMPLES,
        presets: CRON_PRESETS,
        default_expr: CRON_DEFAULT_EXPR,
        default_desc: CRON_DEFAULT_DESC,
        canonical_url,
        og,
        version: env!("CARGO_PKG_VERSION"),
    };
    let body = page
        .render()
        .unwrap_or_else(|e| format!("<!-- cron render failed: {e} -->"));
    cached_render(body)
}

async fn cron(State(cfg): State<Arc<MarketingCfg>>, headers: HeaderMap) -> Response {
    let cached = CRON_CACHED.get_or_init(|| render_cron(&cfg));
    serve_cached(&headers, cached, &TOOL_CACHE_CONTROL)
}

/// Path + copy for each tool, so the sitemap and llms index iterate one list.
pub struct ToolMeta {
    pub path: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

pub const TOOLS: &[ToolMeta] = &[
    ToolMeta {
        path: UPTIME_SLA_PATH,
        title: UPTIME_SLA_TITLE,
        description: UPTIME_SLA_DESCRIPTION,
    },
    ToolMeta {
        path: CRON_PATH,
        title: CRON_TITLE,
        description: CRON_DESCRIPTION,
    },
];

/// Mount every tool route.
pub fn mount(router: axum::Router<Arc<MarketingCfg>>) -> axum::Router<Arc<MarketingCfg>> {
    router
        .route(UPTIME_SLA_PATH, axum::routing::get(uptime_sla))
        .route(CRON_PATH, axum::routing::get(cron))
}

pub(crate) fn warm(cfg: &MarketingCfg) {
    UPTIME_SLA_CACHED.get_or_init(|| render_uptime_sla(cfg));
    CRON_CACHED.get_or_init(|| render_cron(cfg));
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

    #[test]
    fn tools_registry_is_seo_clean() {
        let paths: Vec<_> = TOOLS.iter().map(|t| t.path).collect();
        assert!(paths.contains(&UPTIME_SLA_PATH));
        assert!(paths.contains(&CRON_PATH));
        for t in TOOLS {
            assert!(
                t.path.starts_with("/tools/"),
                "{} not under /tools/",
                t.path
            );
            let rendered_title = t.title.len() + " | ".len() + BRAND.len();
            assert!(
                rendered_title <= 60,
                "{} title {rendered_title} > 60",
                t.path
            );
            assert!(
                t.description.len() <= 160,
                "{} description {} > 160",
                t.path,
                t.description.len()
            );
        }
    }
}
