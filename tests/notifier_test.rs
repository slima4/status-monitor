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
use uptimepage::domain::{ChannelConfig, CheckStatus};
use uptimepage::http_outbound::build_outbound_client;
use uptimepage::notifier::build_notifier;
use uptimepage::notifier::event::{AlertEvent, AlertKind};
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

fn make_event() -> AlertEvent {
    AlertEvent {
        target_id: Uuid::now_v7(),
        target_name: "demo".into(),
        kind: AlertKind::Down,
        consecutive_failures: 3,
        last_status: CheckStatus::Down,
        last_error: Some("500".into()),
        timestamp: Utc::now(),
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
    notifier.notify(&make_event()).await.expect("notify");

    let captured = store.lock().clone();
    assert_eq!(captured.len(), 1);
    let text = captured[0]
        .body
        .get("text")
        .and_then(Value::as_str)
        .unwrap();
    assert!(text.contains("DOWN"));
    assert!(text.contains("demo"));
}

#[tokio::test]
async fn webhook_channel_posts_event_payload_with_custom_header() {
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
    notifier.notify(&make_event()).await.expect("notify");

    let captured = store.lock().clone();
    assert_eq!(captured.len(), 1);
    let body = &captured[0].body;
    // The event no longer carries a `channel` discriminator.
    assert!(body.get("channel").is_none());
    assert_eq!(body["kind"], "down");
    assert_eq!(body["target_name"], "demo");
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
