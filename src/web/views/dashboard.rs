use askama::Template;
use askama_web::WebTemplate;
use axum::Json;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use crate::api::handlers::dashboard::dashboard_summary;
use crate::api::types::DashboardSummary;
use crate::app::AppState;
use crate::web::assets::filters;
use crate::web::error::WebResult;
use crate::web::host::is_subdomain_public_request;
use crate::web::views::public_status::{self, StatusParams};
use crate::web::{AuthedBrowser, CurrentOrg};

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

/// `/` dispatcher: SaaS subdomain hosts (`{slug}.{base_domain}`) serve the
/// per-org public status page at apex; every other host falls through to the
/// operator dashboard. The branch lives here rather than as router-level
/// middleware because axum's `Router::layer` runs *after* path matching —
/// rewriting the URI in a layer would never re-route to `/status`. Single
/// `/` handler picks the surface once.
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
    // Operator / self-host dashboard. Run the same extractors `index` would
    // have run via the router so their rejection paths (login redirect, org
    // error envelope) are byte-identical to the previous direct mount.
    let auth = match AuthedBrowser::from_request_parts(&mut parts, app_state).await {
        Ok(a) => a,
        Err(rej) => return rej.into_response(),
    };
    let org = match CurrentOrg::from_request_parts(&mut parts, app_state).await {
        Ok(o) => o,
        Err(rej) => return rej.into_response(),
    };
    match index(auth, state, org).await {
        Ok(page) => page.into_response(),
        Err(e) => e.into_response(),
    }
}

pub async fn index(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    org: CurrentOrg,
) -> WebResult<DashboardPage> {
    let summary = load_summary(state, org).await?;
    let uptime_pct = format!("{:.2}", summary.last_24h.uptime_pct);
    Ok(DashboardPage {
        active_tab: "dashboard",
        summary,
        uptime_pct,
    })
}

pub async fn region(
    _auth: AuthedBrowser,
    State(state): State<AppState>,
    org: CurrentOrg,
) -> WebResult<DashboardRegion> {
    let summary = load_summary(state, org).await?;
    let uptime_pct = format!("{:.2}", summary.last_24h.uptime_pct);
    Ok(DashboardRegion {
        summary,
        uptime_pct,
    })
}

// Bridge into the API handler so the 5-second `state.dashboard_cache`
// stays shared between JSON callers and the web partial poll.
async fn load_summary(
    state: AppState,
    org: CurrentOrg,
) -> Result<DashboardSummary, crate::error::AppError> {
    let Json(summary) = dashboard_summary(State(state), org).await?;
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
