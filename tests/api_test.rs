mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::build_test_app;
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> axum::Router {
    build_test_app(|_| {})
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

fn http_with_auth(name: &str, auth_field: &str, auth_value: Value) -> Value {
    let mut payload = json!({
        "name": name,
        "check": {
            "type": "http",
            "url": "http://example.com/",
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
    });
    payload["check"][auth_field] = auth_value;
    payload
}

fn http_with_basic_auth() -> Value {
    http_with_auth("with-basic", "basic_auth", json!(["alice", "s3cret"]))
}

fn http_with_bearer() -> Value {
    http_with_auth("with-bearer", "bearer_token", json!("tok.en.value"))
}

async fn post_and_body(app: axum::Router, payload: Value) -> Value {
    let resp = app
        .oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    body_json(resp).await
}

#[tokio::test]
async fn redacts_basic_auth_in_create_response() {
    let body = post_and_body(app(), http_with_basic_auth()).await;
    assert_eq!(body["check"]["basic_auth"], json!(["***", "***"]));
}

#[tokio::test]
async fn redacts_bearer_token_in_create_response() {
    let body = post_and_body(app(), http_with_bearer()).await;
    assert_eq!(body["check"]["bearer_token"], json!("***"));
}

#[tokio::test]
async fn redacts_basic_auth_in_get_response() {
    let app = app();
    let created = post_and_body(app.clone(), http_with_basic_auth()).await;
    let id = created["id"].as_str().unwrap();
    let resp = app
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["check"]["basic_auth"], json!(["***", "***"]));
}

#[tokio::test]
async fn rejects_redaction_sentinel_in_basic_auth() {
    let mut payload = http_with_basic_auth();
    payload["check"]["basic_auth"] = json!(["***", "***"]);
    assert_eq!(post_target(payload).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_redaction_sentinel_in_bearer_token() {
    let mut payload = http_with_bearer();
    payload["check"]["bearer_token"] = json!("***");
    assert_eq!(post_target(payload).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_verify_tls_false_with_basic_auth_over_https() {
    let mut payload = http_with_basic_auth();
    payload["check"]["url"] = json!("https://example.com/");
    payload["check"]["verify_tls"] = json!(false);
    assert_eq!(post_target(payload).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_verify_tls_false_with_bearer_over_https() {
    let mut payload = http_with_bearer();
    payload["check"]["url"] = json!("https://example.com/");
    payload["check"]["verify_tls"] = json!(false);
    assert_eq!(post_target(payload).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn accepts_verify_tls_true_with_credentials_over_https() {
    let mut payload = http_with_basic_auth();
    payload["check"]["url"] = json!("https://example.com/");
    payload["check"]["verify_tls"] = json!(true);
    assert_eq!(post_target(payload).await, StatusCode::CREATED);
}

#[tokio::test]
async fn accepts_verify_tls_false_without_credentials() {
    let payload = json!({
        "name": "no-creds-self-signed",
        "check": {
            "type": "http",
            "url": "https://example.com/",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": false
        },
        "interval": 60,
        "tags": []
    });
    assert_eq!(post_target(payload).await, StatusCode::CREATED);
}
