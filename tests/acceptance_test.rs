//! Coverage for the acceptance criteria listed in OPENAPI-spec.md §11 that
//! aren't already exercised by the other integration tests in this directory.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use common::build_test_app;
use serde_json::{Value, json};
use status_monitor::domain::{CheckResult, CheckStatus};
use status_monitor::storage::{InMemorySink, ResultSink, TimeRange};
use tower::ServiceExt;
use uuid::Uuid;

fn app() -> axum::Router {
    build_test_app(|_| {})
}

async fn body_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn json_responses_include_utf8_charset() {
    let resp = app()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "application/json; charset=utf-8");
}

#[tokio::test]
async fn empty_tags_endpoint_returns_empty_envelope() {
    let resp = app()
        .oneshot(Request::get("/api/v1/tags").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 0);
    assert_eq!(v["total"], 0);
}

#[tokio::test]
async fn idempotency_key_replays_bulk_action() {
    let app = app();

    // Two targets to operate on
    let payload = json!({
        "name": "idem-a",
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
        "interval": 60
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
    let id = body_json(resp).await["id"].as_str().unwrap().to_string();

    let action = json!({"ids": [id], "action": {"type": "disable"}}).to_string();

    // First call: original execution
    let r1 = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .header("idempotency-key", "client-key-1")
                .body(Body::from(action.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let body1 = body_json(r1).await;

    // Second call with same key + body: replay (response body identical)
    let r2 = app
        .clone()
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .header("idempotency-key", "client-key-1")
                .body(Body::from(action.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let body2 = body_json(r2).await;
    assert_eq!(body1, body2);

    // Different key: fresh execution, but result body shape still valid.
    let r3 = app
        .oneshot(
            Request::post("/api/v1/targets/bulk-action")
                .header("content-type", "application/json")
                .header("idempotency-key", "client-key-2")
                .body(Body::from(action))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r3.status(), StatusCode::OK);
}

#[tokio::test]
async fn coalescing_separates_runs_split_by_an_up_check() {
    // Build a sink with results: down, down, up, down — should coalesce into
    // TWO incidents (the `up` resets the run).
    use status_monitor::storage::ResultsStore;
    let sink = Arc::new(InMemorySink::new());
    let target_id = Uuid::now_v7();
    let base = Utc::now() - Duration::try_minutes(10).unwrap();
    let mk = |i: i64, st: CheckStatus| CheckResult {
        target_id,
        timestamp: base + Duration::try_seconds(i * 60).unwrap(),
        status: st,
        duration_ms: 10,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: None,
        response_code: None,
        response_size: None,
        error: Some("err".into()),
    };
    sink.write_batch(&[
        mk(0, CheckStatus::Down),
        mk(1, CheckStatus::Down),
        mk(2, CheckStatus::Up),
        mk(3, CheckStatus::Down),
    ])
    .await
    .unwrap();
    let range = TimeRange {
        from: base - Duration::try_seconds(1).unwrap(),
        to: base + Duration::try_minutes(5).unwrap(),
    };
    let store: Arc<dyn ResultsStore> = sink;
    let incidents = store
        .list_incidents(target_id, range, false, 100, 0)
        .await
        .unwrap();
    assert_eq!(
        incidents.len(),
        2,
        "expected two incidents, got {incidents:?}"
    );
    assert!(
        incidents.iter().any(|i| i.ended_at.is_none()),
        "trailing run still ongoing"
    );
}

#[tokio::test]
async fn openapi_json_carries_charset_suffix() {
    let resp = app()
        .oneshot(
            Request::get("/api/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "application/json; charset=utf-8");
}

#[tokio::test]
async fn post_target_returns_location_header() {
    let app = app();
    let payload = json!({
        "name": "loc-test",
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
        "interval": 60
    });
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
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("Location header on 201")
        .to_string();
    let cache_control = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let v = body_json(resp).await;
    let id = v["id"].as_str().unwrap();
    assert_eq!(location, format!("/api/v1/targets/{id}"));
    assert_eq!(cache_control, "no-store");
}

#[tokio::test]
async fn cache_control_set_per_route_kind() {
    let app = app();

    let dash = app
        .clone()
        .oneshot(
            Request::get("/api/v1/dashboard/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dash.status(), StatusCode::OK);
    assert_eq!(
        dash.headers().get("cache-control").unwrap(),
        "private, max-age=5"
    );

    let list = app
        .clone()
        .oneshot(Request::get("/api/v1/targets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(
        list.headers().get("cache-control").unwrap(),
        "private, max-age=10"
    );

    let health = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        health.headers().get("cache-control").is_none(),
        "health endpoints should not pin Cache-Control"
    );
}

#[tokio::test]
async fn check_now_force_bypasses_breaker() {
    // Use a TCP check pointing at a guaranteed-closed loopback port so the
    // worker returns an error; repeating it trips the breaker (threshold 2 in
    // default test config).
    let app = build_test_app(|cfg| {
        cfg.security.allow_private_targets = true;
        cfg.circuit_breaker.failure_threshold = 1;
        cfg.circuit_breaker.open_duration_secs = 60;
        cfg.circuit_breaker.half_open_max_calls = 1;
        cfg.circuit_breaker.success_threshold = 1;
    });

    let payload = json!({
        "name": "breaker-target",
        "check": {
            "type": "tcp",
            "host": "127.0.0.1",
            "port": 1,
            "timeout": 200
        },
        "interval": 60
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
    let id = body_json(resp).await["id"].as_str().unwrap().to_string();

    // First check fails and trips the breaker (failure_threshold = 1).
    let r1 = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/targets/{id}/check-now"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    // Second attempt without force: 422 CIRCUIT_OPEN.
    let r2 = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/targets/{id}/check-now"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = body_json(r2).await;
    assert_eq!(v["error"]["code"], "CIRCUIT_OPEN");

    // With ?force=true: 200, breaker bypassed.
    let r3 = app
        .oneshot(
            Request::post(format!("/api/v1/targets/{id}/check-now?force=true"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r3.status(), StatusCode::OK);
}
