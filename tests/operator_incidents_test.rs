mod common;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::{body_json, build_test_app_with_seedable_incidents, json_request};
use serde_json::json;
use tower::ServiceExt;
use uptimepage::domain::{CheckStatus, Incident, IncidentSeverity};
use uuid::Uuid;

fn seed_incident() -> Incident {
    Incident {
        id: Uuid::now_v7(),
        target_id: Uuid::now_v7(),
        started_at: Utc::now() - Duration::minutes(10),
        ended_at: None,
        status: CheckStatus::Down,
        duration_secs: None,
        check_count: 3,
        error_sample: None,
        severity: IncidentSeverity::Major,
        public_title: None,
        public_description: None,
        created_at: Some(Utc::now() - Duration::minutes(10)),
        updated_at: Some(Utc::now() - Duration::minutes(10)),
        updates: vec![],
    }
}

#[tokio::test]
async fn patch_narration_sets_title_and_severity() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let inc = seed_incident();
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/incidents/{id}"),
            json!({
                "public_title": "Latency spike",
                "severity": "critical"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["public_title"], "Latency spike");
    assert_eq!(v["severity"], "critical");
}

#[tokio::test]
async fn patch_narration_null_clears_title() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let mut inc = seed_incident();
    inc.public_title = Some("old".into());
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/incidents/{id}"),
            json!({ "public_title": null }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["public_title"].is_null());
}

#[tokio::test]
async fn patch_narration_unknown_id_404() {
    let (app, _) = build_test_app_with_seedable_incidents(|_| {});
    let resp = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/incidents/{}", Uuid::now_v7()),
            json!({ "public_title": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(resp).await["error"]["code"], "INCIDENT_NOT_FOUND");
}

#[tokio::test]
async fn patch_narration_empty_title_rejected() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let inc = seed_incident();
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "PATCH",
            &format!("/api/v1/incidents/{id}"),
            json!({ "public_title": "   " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["error"]["code"], "EMPTY_TITLE");
}

#[tokio::test]
async fn post_update_appends_to_timeline() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let inc = seed_incident();
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/incidents/{id}/updates"),
            json!({ "phase": "identified", "message": "rolling back" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(location, format!("/api/v1/incidents/{id}"));
    let v = body_json(resp).await;
    assert_eq!(v["phase"], "identified");
    assert_eq!(v["message"], "rolling back");
}

#[tokio::test]
async fn post_update_empty_message_rejected() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let inc = seed_incident();
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/incidents/{id}/updates"),
            json!({ "phase": "investigating", "message": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["error"]["code"], "EMPTY_MESSAGE");
}

#[tokio::test]
async fn post_update_long_message_rejected() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let inc = seed_incident();
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/incidents/{id}/updates"),
            json!({ "phase": "monitoring", "message": "x".repeat(2_001) }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["error"]["code"], "MESSAGE_TOO_LONG");
}

#[tokio::test]
async fn post_update_unknown_incident_404() {
    let (app, _) = build_test_app_with_seedable_incidents(|_| {});
    let resp = app
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/incidents/{}/updates", Uuid::now_v7()),
            json!({ "phase": "investigating", "message": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(resp).await["error"]["code"], "INCIDENT_NOT_FOUND");
}

#[tokio::test]
async fn post_update_invalid_phase_rejected_by_serde() {
    let (app, store) = build_test_app_with_seedable_incidents(|_| {});
    let inc = seed_incident();
    let id = inc.id;
    store.seed(inc);
    let resp = app
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/incidents/{id}/updates"),
            json!({ "phase": "loitering", "message": "x" }),
        ))
        .await
        .unwrap();
    // axum's Json extractor returns 422 for malformed JSON / unknown enum
    // variants. 400 is reserved for handler-level validation.
    assert!(
        resp.status() == StatusCode::BAD_REQUEST
            || resp.status() == StatusCode::UNPROCESSABLE_ENTITY,
        "phase validation should be 4xx; got {}",
        resp.status()
    );
}
