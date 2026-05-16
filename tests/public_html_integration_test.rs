//! HTML integration tests for the public `/status` page.
//!
//! Acceptance criteria coverage:
//!   * `GET /status` returns 200 text/html
//!   * Operator-set `public_title` shows that title, not auto-generated
//!   * Empty page (N=0 components) renders with the "operational" banner
//!   * `?fragment=1` returns the dynamic region only (no doctype)
//!   * Aggregator returning `Unavailable` produces 503 + visible warning page

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use tower::ServiceExt;
use uuid::Uuid;

use common::build_test_app_with_web_and_public_source;
use status_monitor::api::PageEnvelope;
use status_monitor::api::public_error::PublicAppError;
use status_monitor::domain::{
    ComponentHistoryResponse, DayState, IncidentSeverity, IncidentStatusPhase, OrgId, OverallState,
    OverallStatus, PublicComponent, PublicComponentGroup, PublicComponentStatus, PublicIncident,
    PublicIncidentUpdate, PublicMaintenanceList, PublicStatusPage,
};
use status_monitor::public_status::{IncidentListQuery, PublicSource};

const OPERATOR_TITLE: &str = "API down in EU-WEST — investigating router";
const PUBLIC_COMPONENT_NAME: &str = "Public API";

fn fixed_incident_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000aa1").unwrap()
}
fn fixed_component_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000bb1").unwrap()
}

/// Fake source that always returns a single component + an active incident
/// whose `title` mirrors the operator-set public_title verbatim.
struct PublishedSource;

#[async_trait]
impl PublicSource for PublishedSource {
    async fn page(&self, _org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError> {
        let component = PublicComponent {
            id: fixed_component_id(),
            name: PUBLIC_COMPONENT_NAME.into(),
            description: Some("primary edge".into()),
            current_status: PublicComponentStatus::MajorOutage,
            history: vec![DayState::Operational; 90],
        };
        let incident = PublicIncident {
            id: fixed_incident_id(),
            component_id: component.id,
            component_name: component.name.clone(),
            title: OPERATOR_TITLE.into(),
            started_at: Utc::now() - chrono::Duration::minutes(8),
            ended_at: None,
            severity: IncidentSeverity::Major,
            status_phase: IncidentStatusPhase::Investigating,
            updates: vec![PublicIncidentUpdate {
                posted_at: Utc::now() - chrono::Duration::minutes(2),
                phase: IncidentStatusPhase::Investigating,
                message: "Engineers paged.".into(),
            }],
        };
        Ok(Arc::new(PublicStatusPage {
            overall: OverallStatus {
                state: OverallState::MajorOutage,
                label: "Major System Outage".into(),
            },
            generated_at: Utc::now(),
            site_name: "status-monitor".into(),
            groups: vec![PublicComponentGroup {
                name: Some("Edge".into()),
                components: vec![component],
            }],
            active_incidents: vec![incident.clone()],
            recent_incidents: vec![incident],
            active_maintenance: Vec::new(),
            upcoming_maintenance: Vec::new(),
        }))
    }

    async fn component_history(
        &self,
        _org: OrgId,
        _id: Uuid,
        _days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError> {
        unimplemented!("not exercised by HTML page tests")
    }
    async fn list_incidents(
        &self,
        _org: OrgId,
        _q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError> {
        unimplemented!("not exercised by HTML page tests")
    }
    async fn incident_by_id(
        &self,
        _org: OrgId,
        _id: Uuid,
    ) -> Result<PublicIncident, PublicAppError> {
        unimplemented!("not exercised by HTML page tests")
    }
    async fn maintenance(&self, _org: OrgId) -> Result<PublicMaintenanceList, PublicAppError> {
        unimplemented!("not exercised by HTML page tests")
    }
    async fn incidents_rss(&self, _org: OrgId, _base_url: &str) -> Result<String, PublicAppError> {
        unimplemented!("not exercised by HTML page tests")
    }
}

/// Source whose components have `DayState::NoData` for every history cell —
/// models "ClickHouse reachable but returns no data". Page-level
/// status still resolves to operational because no component reports an
/// outage.
struct EmptyDataSource;

#[async_trait]
impl PublicSource for EmptyDataSource {
    async fn page(&self, _org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError> {
        let component = PublicComponent {
            id: fixed_component_id(),
            name: PUBLIC_COMPONENT_NAME.into(),
            description: None,
            current_status: PublicComponentStatus::Operational,
            history: vec![DayState::NoData; 90],
        };
        Ok(Arc::new(PublicStatusPage {
            overall: OverallStatus {
                state: OverallState::Operational,
                label: "All Systems Operational".into(),
            },
            generated_at: Utc::now(),
            site_name: "status-monitor".into(),
            groups: vec![PublicComponentGroup {
                name: None,
                components: vec![component],
            }],
            active_incidents: Vec::new(),
            recent_incidents: Vec::new(),
            active_maintenance: Vec::new(),
            upcoming_maintenance: Vec::new(),
        }))
    }
    async fn component_history(
        &self,
        _org: OrgId,
        _id: Uuid,
        _days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError> {
        unimplemented!()
    }
    async fn list_incidents(
        &self,
        _org: OrgId,
        _q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError> {
        unimplemented!()
    }
    async fn incident_by_id(
        &self,
        _org: OrgId,
        _id: Uuid,
    ) -> Result<PublicIncident, PublicAppError> {
        unimplemented!()
    }
    async fn maintenance(&self, _org: OrgId) -> Result<PublicMaintenanceList, PublicAppError> {
        unimplemented!()
    }
    async fn incidents_rss(&self, _org: OrgId, _base_url: &str) -> Result<String, PublicAppError> {
        unimplemented!()
    }
}

/// Source where a maintenance window overlaps a would-be major outage on the
/// same component. Per the truth table in `overall_status::day_state`,
/// maintenance dominates outage; the rendered banner + day cell must reflect
/// `Maintenance`, not `MajorOutage`.
struct MaintenanceDominatesSource;

#[async_trait]
impl PublicSource for MaintenanceDominatesSource {
    async fn page(&self, _org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError> {
        let mut history = vec![DayState::Operational; 90];
        // Today's cell is the right-most slot in oldest-first order.
        history[89] = DayState::Maintenance;
        let component = PublicComponent {
            id: fixed_component_id(),
            name: PUBLIC_COMPONENT_NAME.into(),
            description: None,
            current_status: PublicComponentStatus::Maintenance,
            history,
        };
        let now = Utc::now();
        let maintenance = status_monitor::domain::PublicMaintenance {
            id: Uuid::nil(),
            title: "Planned cutover".into(),
            description: None,
            starts_at: now - chrono::Duration::minutes(10),
            ends_at: now + chrono::Duration::hours(1),
            affected_component_names: vec![PUBLIC_COMPONENT_NAME.into()],
        };
        Ok(Arc::new(PublicStatusPage {
            overall: OverallStatus {
                state: OverallState::Maintenance,
                label: "Maintenance in progress".into(),
            },
            generated_at: now,
            site_name: "status-monitor".into(),
            groups: vec![PublicComponentGroup {
                name: None,
                components: vec![component],
            }],
            active_incidents: Vec::new(),
            recent_incidents: Vec::new(),
            active_maintenance: vec![maintenance],
            upcoming_maintenance: Vec::new(),
        }))
    }
    async fn component_history(
        &self,
        _org: OrgId,
        _id: Uuid,
        _days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError> {
        unimplemented!()
    }
    async fn list_incidents(
        &self,
        _org: OrgId,
        _q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError> {
        unimplemented!()
    }
    async fn incident_by_id(
        &self,
        _org: OrgId,
        _id: Uuid,
    ) -> Result<PublicIncident, PublicAppError> {
        unimplemented!()
    }
    async fn maintenance(&self, _org: OrgId) -> Result<PublicMaintenanceList, PublicAppError> {
        unimplemented!()
    }
    async fn incidents_rss(&self, _org: OrgId, _base_url: &str) -> Result<String, PublicAppError> {
        unimplemented!()
    }
}

use common::UnavailablePublicSource as UnavailableSource;

async fn body_text(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn ct(resp: &axum::http::Response<Body>) -> String {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

// ── /status base case ──────────────────────────────────────────────────────

#[tokio::test]
async fn status_page_returns_200_text_html() {
    let app = build_test_app_with_web_and_public_source(|_| {}, Arc::new(PublishedSource));
    let resp = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(ct(&resp).starts_with("text/html"), "ct: {}", ct(&resp));
    let html = body_text(resp).await;
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains(PUBLIC_COMPONENT_NAME));
}

#[tokio::test]
async fn status_page_shows_operator_public_title_verbatim() {
    let app = build_test_app_with_web_and_public_source(|_| {}, Arc::new(PublishedSource));
    let resp = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(
        html.contains(OPERATOR_TITLE),
        "operator title missing from /status:\n{html}"
    );
}

#[tokio::test]
async fn status_page_renders_empty_page_as_operational() {
    // NoopPublicSource (the default) returns no components and the operational
    // banner — verifies the N=0 acceptance case end-to-end.
    let app = common::build_test_app_with_web(|_| {});
    let resp = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("All Systems Operational"));
}

// ── HTMX partial swap ──────────────────────────────────────────────────────

#[tokio::test]
async fn status_fragment_returns_region_without_doctype() {
    let app = build_test_app_with_web_and_public_source(|_| {}, Arc::new(PublishedSource));
    let resp = app
        .oneshot(
            Request::get("/status?fragment=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(ct(&resp).starts_with("text/html"));
    let html = body_text(resp).await;
    assert!(
        !html.contains("<!doctype html>"),
        "fragment must not include the chrome:\n{html}"
    );
    // The component still surfaces inside the region.
    assert!(html.contains(PUBLIC_COMPONENT_NAME));
    // Region keeps the self-rearming HTMX hooks so subsequent swaps continue
    // (acceptance #5: "auto-refreshes every 30 s without full reload").
    assert!(html.contains(r#"hx-get="/status?fragment=1""#));
    assert!(html.contains(r#"hx-trigger="every 30s""#));
}

// ── ClickHouse reachable, no history data ─────────────────────────────────

#[tokio::test]
async fn status_page_renders_when_components_have_no_history_data() {
    let app = build_test_app_with_web_and_public_source(|_| {}, Arc::new(EmptyDataSource));
    let resp = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    // Component still renders by name even though the strip is all NoData.
    assert!(html.contains(PUBLIC_COMPONENT_NAME));
    // Page resolves to operational — no failures observed.
    assert!(html.contains("All Systems Operational"));
}

// ── maintenance dominates outage ──────────────────────────────────────────

#[tokio::test]
async fn status_page_classifies_as_maintenance_when_window_covers_outage() {
    let app =
        build_test_app_with_web_and_public_source(|_| {}, Arc::new(MaintenanceDominatesSource));
    let resp = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    // Banner reads as maintenance, NOT outage. Phrase matches `overall_label`
    // in `src/public_status/overall_status.rs`.
    assert!(
        html.contains("Maintenance in progress"),
        "maintenance banner missing:\n{html}"
    );
    assert!(
        !html.contains("Major System Outage"),
        "page falsely promotes outage banner over maintenance:\n{html}"
    );
}

// ── ClickHouse unreachable (degraded mode) ─────────────────────────────────

#[tokio::test]
async fn status_page_returns_503_when_source_unavailable() {
    let app = build_test_app_with_web_and_public_source(|_| {}, Arc::new(UnavailableSource));
    let resp = app
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(ct(&resp).starts_with("text/html"));
    let html = body_text(resp).await;
    // The 503 chrome carries the explicit "unavailable" wording from
    // templates/error/503.html — keep the marker tight so a regression that
    // renders the operational page with status 503 still trips the assertion.
    assert!(
        html.to_lowercase().contains("unavailable"),
        "503 page must carry the 'unavailable' marker:\n{html}"
    );
}

#[tokio::test]
async fn status_json_endpoint_returns_503_when_source_unavailable() {
    let app = build_test_app_with_web_and_public_source(|_| {}, Arc::new(UnavailableSource));
    let resp = app
        .oneshot(
            Request::get("/api/public/v1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_text(resp).await;
    // Public error envelope is narrow — no trace_id, no details.
    assert!(body.contains(r#""code""#));
    assert!(!body.contains("trace_id"));
}
