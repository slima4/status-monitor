use askama::Template;
use askama_web::WebTemplate;
use axum::Json;
use axum::extract::State;

use crate::api::handlers::dashboard::dashboard_summary;
use crate::api::types::DashboardSummary;
use crate::app::AppState;
use crate::web::error::WebResult;

#[derive(Template, WebTemplate)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub active_tab: &'static str,
    pub summary: DashboardSummary,
    pub uptime_pct: String,
}

#[derive(Template, WebTemplate)]
#[template(path = "dashboard/region.html")]
pub struct DashboardRegion {
    pub summary: DashboardSummary,
    pub uptime_pct: String,
}

pub async fn index(State(state): State<AppState>) -> WebResult<DashboardPage> {
    let summary = load_summary(state).await?;
    let uptime_pct = format!("{:.2}", summary.last_24h.uptime_pct);
    Ok(DashboardPage {
        active_tab: "dashboard",
        summary,
        uptime_pct,
    })
}

pub async fn region(State(state): State<AppState>) -> WebResult<DashboardRegion> {
    let summary = load_summary(state).await?;
    let uptime_pct = format!("{:.2}", summary.last_24h.uptime_pct);
    Ok(DashboardRegion {
        summary,
        uptime_pct,
    })
}

// Bridge into the API handler so the 5-second `state.dashboard_cache`
// stays shared between JSON callers and the web partial poll.
async fn load_summary(state: AppState) -> Result<DashboardSummary, crate::error::AppError> {
    let Json(summary) = dashboard_summary(State(state)).await?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Last24hSummary, StatusBreakdown, SystemSummary, TargetsSummary};

    fn sample_summary() -> DashboardSummary {
        DashboardSummary {
            targets: TargetsSummary {
                total: 42,
                enabled: 40,
                disabled: 2,
            },
            current_status: StatusBreakdown {
                up: 38,
                down: 1,
                degraded: 1,
                error: 0,
                unknown: 2,
            },
            last_24h: Last24hSummary {
                checks_total: 50_400,
                checks_up: 50_360,
                uptime_pct: 99.92,
                incidents: 3,
            },
            system: SystemSummary {
                in_flight_checks: 5,
                result_queue_depth: 12,
                dropped_results_last_5m: 0,
                circuit_breakers_open: 0,
            },
        }
    }

    fn sample_page() -> DashboardPage {
        let summary = sample_summary();
        let uptime_pct = format!("{:.2}", summary.last_24h.uptime_pct);
        DashboardPage {
            active_tab: "dashboard",
            summary,
            uptime_pct,
        }
    }

    fn sample_region() -> DashboardRegion {
        let summary = sample_summary();
        let uptime_pct = format!("{:.2}", summary.last_24h.uptime_pct);
        DashboardRegion {
            summary,
            uptime_pct,
        }
    }

    #[test]
    fn dashboard_page_renders_chrome_kpis_and_charts() {
        let html = sample_page().render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Dashboard"));
        assert!(html.contains("42"));
        assert!(html.contains("99.92"));
        assert!(html.contains(r#"hx-get="/web/partials/dashboard""#));
        assert!(html.contains(r#"hx-trigger="every 5s""#));
        assert!(html.contains(r#"id="status-donut""#));
        assert!(html.contains(r#"id="last24h-bar""#));
        assert!(html.contains(r#"data-endpoint="/api/v1/dashboard/summary""#));
        assert!(html.contains("/static/js/echarts.min.js"));
        assert!(html.contains("/static/js/charts/dashboard.js"));
    }

    #[test]
    fn dashboard_region_renders_chrome_free_fragment() {
        let html = sample_region().render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(!html.contains("<nav"));
        assert!(html.contains(r#"id="dashboard-region""#));
        assert!(html.contains(r#"hx-get="/web/partials/dashboard""#));
        assert!(html.contains("99.92"));
    }
}
