//! Extractor unit tests for `CurrentOrg`. Covers the in-memory branches:
//! self-host short-circuit, SaaS without a session, and the
//! tenancy-enabled-but-no-db misconfiguration. SaaS branches that require a
//! real Postgres (`is_active_member`, personal-org fallback) are exercised in
//! the live-DB suite once the auth backend lands.

mod common;

use axum::extract::FromRequestParts;
use axum::http::Request;
use status_monitor::error::AppError;
use status_monitor::web::auth::CurrentOrg;

#[tokio::test]
async fn self_host_mode_returns_default_org() {
    let state = common::build_test_app_state(|cfg| cfg.tenancy.enabled = false);
    let (mut parts, _) = Request::builder().uri("/").body(()).unwrap().into_parts();
    let CurrentOrg(id) = CurrentOrg::from_request_parts(&mut parts, &state)
        .await
        .expect("self-host extractor is infallible");
    assert_eq!(id, state.default_org_id);
}

#[tokio::test]
async fn saas_mode_without_session_returns_unauthorized() {
    let state = common::build_test_app_state(|cfg| cfg.tenancy.enabled = true);
    let (mut parts, _) = Request::builder().uri("/").body(()).unwrap().into_parts();
    let err = CurrentOrg::from_request_parts(&mut parts, &state)
        .await
        .expect_err("SaaS without session must reject");
    assert!(
        matches!(err, AppError::Unauthorized),
        "expected Unauthorized, got {err:?}"
    );
}

/// Today the SaaS-mode session is always empty (auth backend isn't wired), so
/// the unauthorized branch fires before the db-is-none check can. Once a real
/// session yields `Some(user)` while a misconfigured deployment leaves
/// `db = None`, the extractor must surface an internal error rather than
/// silently grant the default org. This test pins the current behaviour; when
/// `Session::from_request_parts` becomes fallible, add a sibling case that
/// builds a session with `user = Some(...)` and asserts `AppError::Other`.
#[tokio::test]
async fn saas_mode_db_none_yields_unauthorized_via_empty_session() {
    let state = common::build_test_app_state(|cfg| cfg.tenancy.enabled = true);
    assert!(state.db.is_none(), "fixture must keep db = None");
    let (mut parts, _) = Request::builder().uri("/").body(()).unwrap().into_parts();
    let err = CurrentOrg::from_request_parts(&mut parts, &state)
        .await
        .expect_err("db=None + tenancy=on + no session must reject");
    assert!(matches!(err, AppError::Unauthorized));
}
