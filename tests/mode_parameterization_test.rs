//! Parameterised integration tests that run the same scenarios in both
//! `tenancy.enabled = false` (self-host) and `tenancy.enabled = true` (SaaS)
//! modes. Catches regressions where a handler implicitly relies on one mode
//! and breaks the other.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TenancyMode, build_test_app, build_test_app_with_web};
use rstest::rstest;
use tower::ServiceExt;

async fn status(app: axum::Router, path: &str) -> StatusCode {
    let resp = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("oneshot");
    resp.status()
}

/// Targets list is a tenant-scoped endpoint. Self-host resolves the org from
/// the default and returns `200 OK` with `[]` (the empty in-memory store);
/// SaaS rejects an anonymous request with 401 — the store is org-scoped, so a
/// caller with no authenticated org cannot enumerate another tenant's targets.
/// Pinning the exact status per mode catches a regression where the route
/// silently 404s/500s in one mode that an `assert_ne!(404)` would miss.
#[rstest]
#[case::self_host(TenancyMode::SelfHost, StatusCode::OK)]
#[case::saas(TenancyMode::Saas, StatusCode::UNAUTHORIZED)]
#[tokio::test]
async fn list_targets_works_in_both_modes(#[case] mode: TenancyMode, #[case] expected: StatusCode) {
    let app = build_test_app(|cfg| mode.apply(cfg));
    assert_eq!(status(app, "/api/v1/targets").await, expected);
}

/// Dashboard summary is mounted in both modes. Self-host resolves the org
/// from the default and returns 200; SaaS rejects an anonymous request with
/// 401 (auth lands in `AUTH-spec.md`). Pinning the *exact* status per mode
/// catches the regression "the route is now silently 500ing in SaaS" that an
/// `assert_ne!(404)` would miss.
#[rstest]
#[case::self_host(TenancyMode::SelfHost, StatusCode::OK)]
#[case::saas(TenancyMode::Saas, StatusCode::UNAUTHORIZED)]
#[tokio::test]
async fn dashboard_summary_status_per_mode(
    #[case] mode: TenancyMode,
    #[case] expected: StatusCode,
) {
    let app = build_test_app(|cfg| mode.apply(cfg));
    assert_eq!(status(app, "/api/v1/dashboard/summary").await, expected);
}

/// Health/readiness probes are tenancy-agnostic. They must respond identically
/// regardless of mode.
#[rstest]
#[case::self_host(TenancyMode::SelfHost)]
#[case::saas(TenancyMode::Saas)]
#[tokio::test]
async fn health_endpoints_respond_in_both_modes(#[case] mode: TenancyMode) {
    let app = build_test_app_with_web(|cfg| mode.apply(cfg));
    assert_eq!(status(app.clone(), "/healthz").await, StatusCode::OK);
    // /readyz is OK or 503 depending on backend state; only assert wired.
    let s = status(app, "/readyz").await;
    assert_ne!(s, StatusCode::NOT_FOUND, "readyz must be mounted");
}
