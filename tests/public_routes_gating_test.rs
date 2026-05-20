//! Verifies the path-based public surface is gated by
//! `tenancy.path_based_public_routes`. Self-host defaults to `true`; SaaS
//! must keep it `false` and route per-org via `*.{base_domain}` instead
//! (apex-wildcard shape; wired in a later phase). Mounting the path-based
//! surface in SaaS would expose the default org's data to every tenant.

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
async fn self_host_mode_serves_path_based_public_routes() {
    let app = build_test_app_with_web(|cfg| {
        cfg.tenancy.enabled = false;
        cfg.tenancy.path_based_public_routes = true;
        cfg.tenancy.subdomain_public_routes = false;
    });
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
async fn saas_mode_with_path_based_off_404s_path_routes() {
    let app = build_test_app_with_web(|cfg| {
        cfg.tenancy.enabled = true;
        cfg.tenancy.path_based_public_routes = false;
        cfg.tenancy.subdomain_public_routes = true;
    });
    assert_eq!(
        status(app.clone(), "/api/public/v1/status").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(status(app, "/status").await, StatusCode::NOT_FOUND);
}
