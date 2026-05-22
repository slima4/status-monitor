mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use common::{body_json, build_test_app_with_owner, json_request};
use serde_json::{Value, json};
use tower::ServiceExt;

fn make_app() -> axum::Router {
    build_test_app_with_owner(|_| {})
}

fn valid_window() -> Value {
    json!({
        "title": "DB upgrade",
        "description": "Brief read-only window.",
        "starts_at": (Utc::now() + Duration::hours(1)).to_rfc3339(),
        "ends_at":   (Utc::now() + Duration::hours(2)).to_rfc3339(),
        "component_ids": []
    })
}

#[tokio::test]
async fn create_maintenance_returns_201_and_location() {
    let app = make_app();
    let resp = app
        .oneshot(json_request("POST", "/api/v1/maintenance", valid_window()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get("location")
        .expect("Location header set")
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with("/api/v1/maintenance/"));
    let v = body_json(resp).await;
    assert_eq!(v["title"], "DB upgrade");
    assert!(v["id"].is_string());
}

#[tokio::test]
async fn create_maintenance_rejects_empty_title() {
    let app = make_app();
    let mut body = valid_window();
    body["title"] = json!("   ");
    let resp = app
        .oneshot(json_request("POST", "/api/v1/maintenance", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "EMPTY_TITLE");
}

#[tokio::test]
async fn create_maintenance_rejects_inverted_time_range() {
    let app = make_app();
    let mut body = valid_window();
    let s = body["starts_at"].clone();
    let e = body["ends_at"].clone();
    body["starts_at"] = e;
    body["ends_at"] = s;
    let resp = app
        .oneshot(json_request("POST", "/api/v1/maintenance", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["error"]["code"], "INVALID_TIME_RANGE");
}

#[tokio::test]
async fn create_maintenance_rejects_long_duration() {
    let app = make_app();
    let mut body = valid_window();
    body["ends_at"] = json!((Utc::now() + Duration::days(45)).to_rfc3339());
    let resp = app
        .oneshot(json_request("POST", "/api/v1/maintenance", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["error"]["code"], "INVALID_DURATION");
}

#[tokio::test]
async fn create_maintenance_rejects_unknown_component_ids() {
    let app = make_app();
    let mut body = valid_window();
    body["component_ids"] = json!(["00000000-0000-0000-0000-000000000001"]);
    let resp = app
        .oneshot(json_request("POST", "/api/v1/maintenance", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(resp).await["error"]["code"],
        "INVALID_COMPONENT_ID"
    );
}

#[tokio::test]
async fn list_maintenance_paginates_and_filters() {
    let app = make_app();
    for _ in 0..3 {
        let _ = app
            .clone()
            .oneshot(json_request("POST", "/api/v1/maintenance", valid_window()))
            .await
            .unwrap();
    }
    let resp = app
        .oneshot(
            Request::get("/api/v1/maintenance?status=upcoming&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["items"].as_array().unwrap().len() >= 3);
    assert_eq!(v["has_more"], false);
}

#[tokio::test]
async fn get_unknown_maintenance_returns_404() {
    let app = make_app();
    let resp = app
        .oneshot(
            Request::get("/api/v1/maintenance/00000000-0000-0000-0000-000000000099")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(resp).await["error"]["code"],
        "MAINTENANCE_NOT_FOUND"
    );
}

#[tokio::test]
async fn delete_maintenance_round_trip() {
    let app = make_app();
    let create = app
        .clone()
        .oneshot(json_request("POST", "/api/v1/maintenance", valid_window()))
        .await
        .unwrap();
    let id = body_json(create).await["id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/maintenance/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // Second delete -> 404
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/maintenance/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_maintenance_changes_title() {
    let app = make_app();
    let create = app
        .clone()
        .oneshot(json_request("POST", "/api/v1/maintenance", valid_window()))
        .await
        .unwrap();
    let id = body_json(create).await["id"].as_str().unwrap().to_string();
    let resp = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/maintenance/{id}"),
            json!({"title": "renamed"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["title"], "renamed");
}
