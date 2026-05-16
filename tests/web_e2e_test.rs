//! End-to-end coverage for the server-rendered UI.
//!
//! Exercises every web route via tower::ServiceExt::oneshot, asserting
//! status code, content-type, and the structural anchors that the JS
//! layer relies on (HTMX hooks, chart data-endpoints, form data-action,
//! credential redaction sentinels).

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::build_test_app_with_web;
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> axum::Router {
    build_test_app_with_web(|_| {})
}

async fn body_text(resp: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(resp.into_body(), 4 << 20).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn html_ct(resp: &axum::http::Response<Body>) -> &str {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

async fn create_http_target(router: &axum::Router, name: &str) -> String {
    let body = json!({
        "name": name,
        "interval": 60,
        "enabled": true,
        "tags": ["e2e"],
        "check": {
            "type": "http",
            "url": "https://example.com/",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true,
            "basic_auth": ["alice", "s3cret"],
            "bearer_token": "tok-abc"
        }
    });
    let resp = router
        .clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create target");
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().expect("id").to_string()
}

#[tokio::test]
async fn dashboard_renders_with_kpi_cards_and_chart_anchors() {
    let resp = app()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(html_ct(&resp).starts_with("text/html"));
    let html = body_text(resp).await;
    assert!(html.contains("Dashboard"));
    assert!(html.contains(r#"id="dashboard-region""#));
    assert!(html.contains(r#"hx-trigger="every 5s""#));
    assert!(html.contains(r#"id="status-donut""#));
    assert!(html.contains(r#"id="last24h-bar""#));
    assert!(html.contains(r#"data-endpoint="/api/v1/dashboard/summary""#));
}

#[tokio::test]
async fn dashboard_partial_returns_chrome_free_fragment() {
    let resp = app()
        .oneshot(
            Request::get("/web/partials/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(!html.contains("<!doctype html>"));
    assert!(!html.contains("<nav"));
    assert!(html.contains(r#"id="dashboard-region""#));
    assert!(html.contains(r#"hx-get="/web/partials/dashboard""#));
}

#[tokio::test]
async fn targets_list_renders_filters_and_table_chrome() {
    let resp = app()
        .oneshot(Request::get("/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("Targets"));
    assert!(html.contains(r#"id="targets-filter""#));
    assert!(html.contains(r#"hx-get="/web/targets/list""#));
    assert!(html.contains(r#"id="target-rows""#));
    assert!(html.contains(r#"scope="col""#));
}

#[tokio::test]
async fn targets_list_partial_returns_tbody_only() {
    let resp = app()
        .oneshot(
            Request::get("/web/targets/list?limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(!html.contains("<!doctype html>"));
    assert!(!html.contains("<nav"));
    assert!(html.contains(r#"id="target-rows""#));
}

#[tokio::test]
async fn new_target_form_renders_create_mode() {
    let resp = app()
        .oneshot(Request::get("/targets/new").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("New target"));
    assert!(html.contains(r#"data-action="/api/v1/targets""#));
    assert!(html.contains(r#"data-method="POST""#));
    assert!(html.contains(r#"data-mode="create""#));
    assert!(html.contains(r#"data-auth-field="basic""#));
    assert!(html.contains(r#"data-initial-mode="create""#));
    assert!(html.contains("Set credentials"));
    assert!(html.contains("Set token"));
}

#[tokio::test]
async fn edit_form_shows_redacted_auth_state_for_existing_target() {
    let router = app();
    let id = create_http_target(&router, "redacted-edit-target").await;

    let resp = router
        .clone()
        .oneshot(
            Request::get(format!("/targets/{id}/edit"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("Edit redacted-edit-target"));
    assert!(html.contains(r#"data-method="PATCH""#));
    assert!(html.contains(r#"data-mode="edit""#));
    assert!(html.contains(r#"data-initial-mode="redacted""#));
    assert!(html.contains("Replace credentials"));
    assert!(html.contains("Replace token"));
    // Real values must NEVER appear in the HTML; only the sentinel does.
    assert!(!html.contains("s3cret"));
    assert!(!html.contains("tok-abc"));
}

#[tokio::test]
async fn target_detail_renders_charts_and_range_nav() {
    let router = app();
    let id = create_http_target(&router, "detail-target").await;

    let resp = router
        .clone()
        .oneshot(
            Request::get(format!("/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_text(resp).await;
    assert!(html.contains("detail-target"));
    assert!(html.contains(r#"aria-label="Time range""#));
    for key in ["1h", "24h", "7d", "30d"] {
        assert!(
            html.contains(&format!("?range={key}")),
            "missing range {key}"
        );
    }
    assert!(html.contains(r#"id="latency-chart""#));
    assert!(html.contains(r#"id="breakdown-chart""#));
    assert!(html.contains("/api/v1/targets/"));
    assert!(html.contains("/static/js/charts/latency.js"));
    assert!(html.contains("/static/js/charts/breakdown.js"));
}

#[tokio::test]
async fn nonexistent_target_detail_returns_html_404() {
    let resp = app()
        .oneshot(
            Request::get("/targets/00000000-0000-0000-0000-000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(html_ct(&resp).starts_with("text/html"));
    let html = body_text(resp).await;
    assert!(html.contains("Not Found"));
}

#[tokio::test]
async fn unknown_web_path_returns_404_html() {
    let resp = app()
        .oneshot(
            Request::get("/this-route-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Cache-control is only `immutable` when the URL is version-pinned (`?v=`,
/// the only form the `asset` filter emits). A bare URL — hand-typed or an
/// old bookmark — gets a short revalidating cache so a content change can't
/// be hidden for a year. This is the e2e mirror of the `web::assets` unit
/// tests; the two must not disagree.
#[tokio::test]
async fn static_assets_cache_control_is_honest() {
    let cache_control = |resp: &axum::http::Response<Body>| {
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };

    let bare = app()
        .oneshot(
            Request::get("/static/css/app.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::OK);
    assert_eq!(
        cache_control(&bare),
        "public, max-age=300",
        "bare asset URL must be short-lived, not immutable",
    );

    let versioned = app()
        .oneshot(
            Request::get("/static/css/app.css?v=deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(versioned.status(), StatusCode::OK);
    assert_eq!(
        cache_control(&versioned),
        "public, max-age=31536000, immutable",
        "version-pinned asset URL must be immutable",
    );
}

#[tokio::test]
async fn recover_account_page_renders_confirm_card() {
    let resp = app()
        .oneshot(
            Request::get("/recover-account?token=tok-abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(html_ct(&resp).starts_with("text/html"));
    let body = body_text(resp).await;
    assert!(body.contains("Recover your account"));
    assert!(body.contains(r#"hx-post="/api/v1/auth/recover-account""#));
    assert!(body.contains(r#"value="tok-abc""#));
}

#[tokio::test]
async fn recover_account_page_blank_token_shows_invalid() {
    let resp = app()
        .oneshot(
            Request::get("/recover-account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_text(resp).await;
    assert!(body.contains("Recovery link invalid"));
    assert!(!body.contains("hx-post"));
}

#[tokio::test]
async fn settings_account_redirects_to_login_when_unauthenticated() {
    let resp = app()
        .oneshot(
            Request::get("/settings/account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "unauthenticated /settings/account must redirect, got {}",
        resp.status()
    );
    let loc = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(loc.starts_with("/login"), "redirect target was {loc}");
}
