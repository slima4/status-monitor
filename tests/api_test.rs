mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use status_monitor::api::build_router;
use status_monitor::app::AppState;
use status_monitor::config::AppConfig;
use status_monitor::storage::{InMemorySink, InMemoryTargetStore};
use tower::ServiceExt;

fn app() -> axum::Router {
    let cfg = AppConfig::load().expect("config");
    let target_store = Arc::new(InMemoryTargetStore::new());
    let results_store = Arc::new(InMemorySink::new());
    let state = AppState::new(cfg, target_store, results_store);
    build_router(state)
}

async fn body_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn healthz_returns_ok() {
    let resp = app()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn readyz_returns_ready_with_inmemory_store() {
    let resp = app()
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "ready");
}

#[tokio::test]
async fn list_targets_empty() {
    let resp = app()
        .oneshot(Request::get("/api/v1/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn create_then_get_then_delete_target() {
    let app = app();
    let payload = json!({
        "name": "ex",
        "check": {
            "type": "http",
            "url": "http://example.com",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true
        },
        "interval": 60,
        "tags": ["prod"]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_json(resp).await;
    assert_eq!(got["name"], "ex");

    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_unknown_target_is_404() {
    let id = uuid::Uuid::now_v7();
    let resp = app()
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bulk_create_rejects_empty() {
    let resp = app()
        .oneshot(
            Request::post("/api/v1/targets/bulk")
                .header("content-type", "application/json")
                .body(Body::from("[]"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

fn ssrf_payload(url: &str) -> Value {
    json!({
        "name": "ssrf-attempt",
        "check": {
            "type": "http",
            "url": url,
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true
        },
        "interval": 60,
        "tags": []
    })
}

async fn post_target(payload: Value) -> StatusCode {
    app()
        .oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn ssrf_rejects_loopback_ipv4_literal() {
    assert_eq!(
        post_target(ssrf_payload("http://127.0.0.1/")).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ssrf_rejects_aws_metadata_literal() {
    assert_eq!(
        post_target(ssrf_payload("http://169.254.169.254/latest/meta-data/")).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ssrf_rejects_private_rfc1918_literal() {
    assert_eq!(
        post_target(ssrf_payload("http://10.0.0.1/")).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post_target(ssrf_payload("http://192.168.1.1/")).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ssrf_rejects_loopback_ipv6_literal() {
    assert_eq!(
        post_target(ssrf_payload("http://[::1]/")).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ssrf_allows_public_hostname() {
    assert_eq!(
        post_target(ssrf_payload("http://example.com/")).await,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn ssrf_rejects_tcp_loopback_literal() {
    let payload = json!({
        "name": "tcp-loopback",
        "check": {
            "type": "tcp",
            "host": "127.0.0.1",
            "port": 22,
            "timeout": 5000
        },
        "interval": 60,
        "tags": []
    });
    assert_eq!(post_target(payload).await, StatusCode::BAD_REQUEST);
}
