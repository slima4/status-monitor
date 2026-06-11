//! API contract for `/api/v1/notification-channels`: CRUD, secret redaction
//! on every read path, the `GET → PATCH` sentinel guard, and the test-send
//! action. In-memory store (no Postgres); cross-tenant isolation and the
//! quota cap are exercised against the live Postgres store separately.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::StatusCode;
use common::{body_json, build_test_app_with_owner, json_request};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app() -> Router {
    build_test_app_with_owner(|_| {})
}

async fn send(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(json_request(method, path, body))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn send_empty(app: &Router, method: &str, path: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let v = if status == StatusCode::NO_CONTENT {
        Value::Null
    } else {
        body_json(resp).await
    };
    (status, v)
}

fn slack_body(name: &str) -> Value {
    json!({
        "name": name,
        "config": { "type": "slack", "webhook_url": "https://hooks.slack.com/services/T/B/secret" }
    })
}

#[tokio::test]
async fn create_redacts_secret_and_sets_location() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/notification-channels",
            slack_body("Ops"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let loc = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert!(loc.starts_with("/api/v1/notification-channels/"));
    let body = body_json(resp).await;
    assert_eq!(body["name"], "Ops");
    assert_eq!(body["kind"], "slack");
    // The webhook URL carries a token, so it must come back masked.
    assert_eq!(body["config"]["webhook_url"], "***");
    assert!(!body.to_string().contains("secret"));
}

#[tokio::test]
async fn list_and_get_keep_secrets_redacted() {
    let app = app();
    let (st, created) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("Ops"),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();

    let (st, list) = send_empty(&app, "GET", "/api/v1/notification-channels").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["config"]["webhook_url"], "***");

    let (st, one) = send_empty(&app, "GET", &format!("/api/v1/notification-channels/{id}")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(one["config"]["webhook_url"], "***");
}

#[tokio::test]
async fn rejects_resubmitted_redaction_sentinel_on_create() {
    let app = app();
    let (st, body) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        json!({
            "name": "Bad",
            "config": { "type": "slack", "webhook_url": "***" }
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "REDACTION_SENTINEL");
}

#[tokio::test]
async fn patch_name_only_keeps_stored_secret() {
    let app = app();
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("Ops"),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    // Round-trip the redacted config back as a PATCH: must be rejected, not
    // written over the real webhook.
    let (st, body) = send(
        &app,
        "PATCH",
        &format!("/api/v1/notification-channels/{id}"),
        json!({ "config": { "type": "slack", "webhook_url": "***" } }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "REDACTION_SENTINEL");

    // Name-only PATCH (config omitted) succeeds and leaves the secret intact.
    let (st, patched) = send(
        &app,
        "PATCH",
        &format!("/api/v1/notification-channels/{id}"),
        json!({ "name": "Ops renamed", "enabled": false }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(patched["name"], "Ops renamed");
    assert_eq!(patched["enabled"], false);
    assert_eq!(patched["config"]["webhook_url"], "***");
}

#[tokio::test]
async fn rejects_non_https_config_and_empty_name() {
    let app = app();
    let (st, body) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        json!({ "name": "Insecure", "config": { "type": "slack", "webhook_url": "http://x" } }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_CHANNEL_CONFIG");

    let (st, body) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        json!({ "name": "  ", "config": { "type": "slack", "webhook_url": "https://hooks.slack.com/x" } }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "CHANNEL_NAME_INVALID");
}

#[tokio::test]
async fn duplicate_name_is_unprocessable() {
    let app = app();
    let (st, _) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("Dup"),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, body) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("Dup"),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "CHANNEL_NAME_TAKEN");
}

/// Deleting a channel must scrub its bindings from `targets.alerts` — a
/// dangling `{channel_id}` fails channel-existence validation on the next
/// whole-array alerts update of that monitor.
#[tokio::test]
async fn delete_scrubs_alert_bindings_from_targets() {
    let app = app();
    let (_, ch_a) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("a"),
    )
    .await;
    let ch_a = ch_a["id"].as_str().unwrap().to_string();

    let (status, target) = send(
        &app,
        "POST",
        "/api/v1/targets",
        json!({
            "name": "api",
            "interval": 60,
            "alerts": [{ "channel_id": ch_a }],
            "check": { "type": "tcp", "host": "example.com", "port": 443, "timeout": 5000 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create target: {target}");
    let target_id = target["id"].as_str().unwrap().to_string();

    let (status, _) = send_empty(
        &app,
        "DELETE",
        &format!("/api/v1/notification-channels/{ch_a}"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send_empty(&app, "GET", &format!("/api/v1/targets/{target_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alerts"], json!([]), "binding must be scrubbed");

    // The regression this guards: with a dangling binding, any later
    // round-tripped alerts update would 400 on the deleted channel id.
    let (_, ch_b) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("b"),
    )
    .await;
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/v1/targets/{target_id}"),
        json!({ "alerts": [{ "channel_id": ch_b["id"].as_str().unwrap() }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rebind must succeed: {body}");
}

#[tokio::test]
async fn delete_then_get_is_404() {
    let app = app();
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/notification-channels",
        slack_body("Ops"),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (st, _) = send_empty(
        &app,
        "DELETE",
        &format!("/api/v1/notification-channels/{id}"),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let (st, body) = send_empty(&app, "GET", &format!("/api/v1/notification-channels/{id}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "CHANNEL_NOT_FOUND");
}

#[tokio::test]
async fn test_config_rejects_invalid_transport() {
    let app = app();
    let (st, body) = send(
        &app,
        "POST",
        "/api/v1/notification-channels/test",
        json!({ "config": { "type": "slack", "webhook_url": "not a url" } }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_CHANNEL_CONFIG");
}

#[tokio::test]
async fn test_config_rejects_redaction_sentinel() {
    let app = app();
    let (st, body) = send(
        &app,
        "POST",
        "/api/v1/notification-channels/test",
        json!({ "config": { "type": "slack", "webhook_url": "***" } }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "REDACTION_SENTINEL");
}

#[tokio::test]
async fn test_send_unknown_channel_is_404() {
    let app = app();
    let (st, body) = send_empty(
        &app,
        "POST",
        "/api/v1/notification-channels/01919b9e-0000-7000-8000-000000000000/test",
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "CHANNEL_NOT_FOUND");
}
