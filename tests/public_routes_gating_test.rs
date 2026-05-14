//! Verifies the spec rule that public-status routes are gated behind
//! `tenancy.public_routes_enabled` in SaaS mode. Self-host mode
//! (`tenancy.enabled = false`) ignores the flag.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::build_test_app_with_web;
use tower::ServiceExt;

async fn status(app: axum::Router, path: &str) -> StatusCode {
    let resp = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("oneshot");
    resp.status()
}

#[tokio::test]
async fn self_host_mode_serves_public_routes_regardless_of_flag() {
    let app = build_test_app_with_web(|cfg| {
        cfg.tenancy.enabled = false;
        cfg.tenancy.public_routes_enabled = false;
    });
    // Either 200 (page rendered) or 503 (no data) — anything but 404 proves
    // the routes are wired.
    let s = status(app.clone(), "/status").await;
    assert_ne!(s, StatusCode::NOT_FOUND, "/status should be mounted");
    let s = status(app, "/api/public/v1/status").await;
    assert_ne!(
        s,
        StatusCode::NOT_FOUND,
        "api public status should be mounted"
    );
}

#[tokio::test]
async fn saas_mode_with_flag_off_404s_public_routes() {
    let app = build_test_app_with_web(|cfg| {
        cfg.tenancy.enabled = true;
        cfg.tenancy.public_routes_enabled = false;
    });
    assert_eq!(
        status(app.clone(), "/api/public/v1/status").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(status(app, "/status").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn saas_mode_with_flag_on_serves_public_routes() {
    let app = build_test_app_with_web(|cfg| {
        cfg.tenancy.enabled = true;
        cfg.tenancy.public_routes_enabled = true;
    });
    let s = status(app.clone(), "/status").await;
    assert_ne!(s, StatusCode::NOT_FOUND);
    let s = status(app, "/api/public/v1/status").await;
    assert_ne!(s, StatusCode::NOT_FOUND);
}
