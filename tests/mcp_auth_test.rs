//! Transport-level auth + gating for the read MCP server at `/mcp`.
//!
//! These cover the unauthenticated discovery path (initialize and the `*_list`
//! methods are open so directories can read the catalog), the no-DB rejection
//! paths (execution and any present-but-invalid token must fail closed), and the
//! `cfg.mcp.enabled` mount gate. The full JSON-RPC tool round-trip is exercised
//! against a live PG+CH via `mcp-remote`/Claude Desktop. See the connector
//! smoke notes.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::build_test_app_with_web;
use tower::ServiceExt;

fn mcp_post_body(authorization: Option<&str>, body: &'static str) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("host", "localhost")
        .header("accept", "application/json, text/event-stream");
    if let Some(a) = authorization {
        b = b.header("authorization", a);
    }
    b.body(Body::from(body)).unwrap()
}

fn mcp_post(authorization: Option<&str>) -> Request<Body> {
    mcp_post_body(
        authorization,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    )
}

const INITIALIZE_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"glama","version":"0"}}}"#;
const TOOLS_CALL_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_org_health","arguments":{}}}"#;

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
async fn mcp_without_token_allows_discovery() {
    // Discovery (initialize) must pass auth with no credential so MCP
    // directories and clients can read the public tool catalog.
    let app = build_test_app_with_web(|cfg| cfg.mcp.enabled = true);
    let resp = app
        .oneshot(mcp_post_body(None, INITIALIZE_BODY))
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated discovery must not be challenged"
    );
    assert!(
        resp.status().is_success(),
        "initialize should succeed without a credential, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn mcp_without_token_blocks_execution() {
    // A tool call with no credential still challenges, so the OAuth step-up
    // fires for real clients.
    let app = build_test_app_with_web(|cfg| cfg.mcp.enabled = true);
    let resp = app
        .oneshot(mcp_post_body(None, TOOLS_CALL_BODY))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers().get("www-authenticate").unwrap(),
        "Bearer",
        "401s must advertise the Bearer scheme for the OAuth step-up"
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
        .oneshot(mcp_post(Some(
            "Bearer sm_live_0000000000000000000000000000000000000000000",
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
