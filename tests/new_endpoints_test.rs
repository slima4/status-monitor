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
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn http_target_payload(name: &str) -> Value {
    json!({
        "name": name,
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
    })
}

async fn create_target(app: &axum::Router, name: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets")
                .header("content-type", "application/json")
                .body(Body::from(http_target_payload(name).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = body_json(resp).await;
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn bulk_action_disable_reports_partial_success() {
    let app = app();
    let id_a = create_target(&app, "a").await;
    let id_b = create_target(&app, "b").await;
    let missing = uuid::Uuid::now_v7().to_string();

    let payload = json!({
        "ids": [id_a, id_b, missing],
        "action": { "type": "disable" }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["succeeded"].as_array().unwrap().len(), 2);
    assert_eq!(v["failed"].as_array().unwrap().len(), 1);
    assert_eq!(v["failed"][0]["code"], "TARGET_NOT_FOUND");
    assert_eq!(v["failed"][0]["id"], missing);
}

#[tokio::test]
async fn bulk_action_rejects_empty_ids() {
    let payload = json!({"ids": [], "action": {"type": "delete"}});
    let resp = app()
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "BULK_EMPTY");
}

#[tokio::test]
async fn bulk_action_tag_add_then_remove() {
    let app = app();
    let id = create_target(&app, "tag-test").await;

    let add = json!({
        "ids": [id],
        "action": { "type": "tag_add", "tags": ["fresh"] }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .body(Body::from(add.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(resp).await;
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(tags.contains(&"fresh"));

    let remove = json!({
        "ids": [id],
        "action": { "type": "tag_remove", "tags": ["fresh"] }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .body(Body::from(remove.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(resp).await;
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert!(!tags.contains(&"fresh"));
}

#[tokio::test]
async fn tags_endpoint_reports_aggregate_counts() {
    let app = app();
    create_target(&app, "a").await;
    create_target(&app, "b").await;

    let resp = app
        .oneshot(Request::get("/api/v1/tags").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let items = v["items"].as_array().unwrap();
    let prod = items
        .iter()
        .find(|t| t["name"] == "prod")
        .expect("prod tag present");
    assert_eq!(prod["count"], 2);
}

#[tokio::test]
async fn check_now_rejects_unknown_target() {
    let id = uuid::Uuid::now_v7();
    let resp = app()
        .oneshot(
            Request::post(format!("/api/v1/targets/{id}/check-now"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "TARGET_NOT_FOUND");
}

#[tokio::test]
async fn test_endpoint_rejects_ssrf_target() {
    let payload = json!({
        "check": {
            "type": "http",
            "url": "http://127.0.0.1/",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": false,
            "max_redirects": 0,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {},
            "verify_tls": true
        }
    });
    let app = build_test_app(|cfg| {
        cfg.security.allow_private_targets = false;
    });
    let resp = app
        .oneshot(
            Request::post("/api/v1/targets/test")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "SSRF_BLOCKED");
}

#[tokio::test]
async fn dashboard_summary_returns_zero_filled_for_empty_fleet() {
    let resp = app()
        .oneshot(
            Request::get("/api/v1/dashboard/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["targets"]["total"], 0);
    assert_eq!(v["last_24h"]["checks_total"], 0);
}

#[tokio::test]
async fn incidents_endpoint_returns_envelope() {
    let app = app();
    let id = create_target(&app, "no-results-yet").await;
    let resp = app
        .oneshot(
            Request::get(format!("/api/v1/targets/{id}/incidents"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 0);
    assert_eq!(v["has_more"], false);
}
