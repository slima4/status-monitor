//! Transport-level auth + gating for the read MCP server at `/mcp`.
//!
//! These cover the no-DB rejection paths (every one must fail closed before a
//! tool runs) and the `cfg.mcp.enabled` mount gate. The full JSON-RPC tool
//! round-trip is exercised against a live PG+CH via `mcp-remote`/Claude Desktop
//! — see the connector smoke notes.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::build_test_app_with_web;
use tower::ServiceExt;

fn mcp_post(authorization: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(a) = authorization {
        b = b.header("authorization", a);
    }
    b.body(Body::from(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    ))
    .unwrap()
}

#[tokio::test]
async fn mcp_disabled_is_not_mounted() {
    // Default config leaves the server off; `/mcp` must 404, not 401.
    let app = build_test_app_with_web(|_| {});
    let resp = app
        .oneshot(mcp_post(Some("Bearer sm_live_anything")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mcp_without_token_challenges() {
    let app = build_test_app_with_web(|cfg| cfg.mcp.enabled = true);
    let resp = app.oneshot(mcp_post(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers().get("www-authenticate").unwrap(),
        "Bearer",
        "401s must advertise the Bearer scheme for the OAuth step-up later"
    );
}

#[tokio::test]
async fn mcp_with_non_sm_live_bearer_challenges() {
    let app = build_test_app_with_web(|cfg| cfg.mcp.enabled = true);
    let resp = app
        .oneshot(mcp_post(Some("Bearer github_pat_xxx")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_with_sm_live_token_but_no_db_challenges() {
    // The in-memory harness has `db: None`; an `sm_live_` token can't be looked
    // up, so the door fails closed rather than letting an unverified token in.
    let app = build_test_app_with_web(|cfg| cfg.mcp.enabled = true);
    let resp = app
        .oneshot(mcp_post(Some("Bearer sm_live_0000000000000000000000000000000000000000000")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
