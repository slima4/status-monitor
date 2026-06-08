mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::Value;
use uptimepage::domain::{
    ChannelConfig, IncidentSeverity, IncidentUrgency, NotificationReason,
};
use uptimepage::http_outbound::build_outbound_client;
use uptimepage::notifier::build_notifier;
use uptimepage::notifier::event::IncidentNotice;
use uuid::Uuid;

#[derive(Default, Clone)]
struct CapturedRequest {
    body: Value,
}

async fn spawn_capture_server() -> (SocketAddr, Arc<Mutex<Vec<CapturedRequest>>>) {
    let store: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/hook", post(capture))
        .with_state(store.clone());
    let addr = crate::common::spawn_router(app).await;
    (addr, store)
}

async fn capture(
    State(store): State<Arc<Mutex<Vec<CapturedRequest>>>>,
    body: String,
) -> StatusCode {
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    store.lock().push(CapturedRequest { body: parsed });
    StatusCode::OK
}

fn make_notice() -> IncidentNotice {
    IncidentNotice {
        incident_id: Uuid::now_v7(),
        reason: NotificationReason::Opened,
        monitor_name: Some("demo".into()),
        title: None,
        severity: IncidentSeverity::Major,
        urgency: IncidentUrgency::High,
        started_at: Utc::now(),
        ended_at: None,
        error_sample: Some("500".into()),
        regions_down: vec![],
        regions_up: vec![],
        url: None,
    }
}

#[tokio::test]
async fn slack_channel_posts_text_payload() {
    let (addr, store) = spawn_capture_server().await;
    let cfg = ChannelConfig::Slack {
        webhook_url: format!("http://{addr}/hook"),
    };
    let notifier = build_notifier(
        &cfg,
        &build_outbound_client(uptimepage::security::SsrfGuard::relaxed_for_tests()),
    )
    .expect("notifier");
    notifier.notify_incident(&make_notice()).await.expect("notify");

    let captured = store.lock().clone();
    assert_eq!(captured.len(), 1);
    let text = captured[0]
        .body
        .get("text")
        .and_then(Value::as_str)
        .unwrap();
    assert!(text.contains("OPEN"));
    assert!(text.contains("demo"));
}

#[tokio::test]
async fn slack_multi_region_includes_breakdown() {
    let (addr, store) = spawn_capture_server().await;
    let cfg = ChannelConfig::Slack {
        webhook_url: format!("http://{addr}/hook"),
    };
    let notifier = build_notifier(
        &cfg,
        &build_outbound_client(uptimepage::security::SsrfGuard::relaxed_for_tests()),
    )
    .expect("notifier");
    let mut notice = make_notice();
    notice.regions_down = vec!["eu-west".into(), "us-east".into()];
    notice.regions_up = vec!["ap-south".into()];
    notifier.notify_incident(&notice).await.expect("notify");

    let captured = store.lock().clone();
    let text = captured[0].body["text"].as_str().unwrap();
    assert!(text.contains("eu-west"), "breakdown missing: {text}");
    assert!(text.contains("ap-south"), "breakdown missing: {text}");
}

#[tokio::test]
async fn slack_single_region_omits_breakdown() {
    let (addr, store) = spawn_capture_server().await;
    let cfg = ChannelConfig::Slack {
        webhook_url: format!("http://{addr}/hook"),
    };
    let notifier = build_notifier(
        &cfg,
        &build_outbound_client(uptimepage::security::SsrfGuard::relaxed_for_tests()),
    )
    .expect("notifier");
    let mut notice = make_notice();
    notice.regions_down = vec!["eu-west".into()];
    notifier.notify_incident(&notice).await.expect("notify");

    let captured = store.lock().clone();
    let text = captured[0].body["text"].as_str().unwrap();
    assert!(!text.contains("down:"), "single region leaked breakdown: {text}");
}

#[tokio::test]
async fn webhook_channel_posts_incident_payload_with_custom_header() {
    let (addr, store) = spawn_capture_server().await;
    let cfg = ChannelConfig::Webhook {
        url: format!("http://{addr}/hook"),
        headers: std::collections::BTreeMap::from([("X-Test-Token".into(), "secret".into())]),
    };
    let notifier = build_notifier(
        &cfg,
        &build_outbound_client(uptimepage::security::SsrfGuard::relaxed_for_tests()),
    )
    .expect("notifier");
    let mut notice = make_notice();
    notice.regions_down = vec!["eu-west".into()];
    notice.regions_up = vec!["us-east".into()];
    notifier.notify_incident(&notice).await.expect("notify");

    let captured = store.lock().clone();
    assert_eq!(captured.len(), 1);
    let body = &captured[0].body;
    assert_eq!(body["monitor_name"], "demo");
    assert_eq!(body["regions_down"][0], "eu-west");
    assert_eq!(body["regions_up"][0], "us-east");
}

#[tokio::test]
async fn build_notifier_constructs_each_kind() {
    let http = build_outbound_client(uptimepage::security::SsrfGuard::relaxed_for_tests());
    assert!(
        build_notifier(
            &ChannelConfig::Telegram {
                bot_token: "123:abc".into(),
                chat_id: "-100".into(),
            },
            &http,
        )
        .is_ok()
    );
    assert!(
        build_notifier(
            &ChannelConfig::Slack {
                webhook_url: "https://hooks.slack.com/x".into(),
            },
            &http,
        )
        .is_ok()
    );
}

#[tokio::test]
async fn build_notifier_rejects_unparseable_url() {
    let http = build_outbound_client(uptimepage::security::SsrfGuard::relaxed_for_tests());
    let err = build_notifier(
        &ChannelConfig::Slack {
            webhook_url: "not a url".into(),
        },
        &http,
    );
    assert!(err.is_err());
}
