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
use status_monitor::config::{NotificationsConfig, SlackConfig, WebhookConfig};
use status_monitor::domain::{AlertChannel, CheckStatus};
use status_monitor::notifier::build_notifiers;
use status_monitor::notifier::event::AlertEvent;
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

fn make_event(channel: AlertChannel, target_id: Uuid) -> AlertEvent {
    use status_monitor::notifier::event::AlertKind;
    AlertEvent {
        target_id,
        target_name: "demo".into(),
        channel,
        kind: AlertKind::Down,
        consecutive_failures: 3,
        last_status: CheckStatus::Down,
        last_error: Some("500".into()),
        timestamp: Utc::now(),
        recipients: vec![],
    }
}

#[tokio::test]
async fn slack_notifier_posts_text_payload() {
    let (addr, store) = spawn_capture_server().await;
    let cfg = NotificationsConfig {
        slack: SlackConfig {
            enabled: true,
            webhook_url: format!("http://{addr}/hook"),
        },
        webhook: WebhookConfig::default(),
        email: Default::default(),
    };
    let notifiers = build_notifiers(&cfg).expect("notifiers");
    assert_eq!(notifiers.len(), 1);

    let event = make_event(AlertChannel::Slack, Uuid::now_v7());
    notifiers[0].notify(&event).await.expect("notify");

    let captured = store.lock().clone();
    assert_eq!(captured.len(), 1);
    let text = captured[0]
        .body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(text.contains("DOWN"));
    assert!(text.contains("demo"));
}

#[tokio::test]
async fn webhook_notifier_posts_event_payload() {
    let (addr, store) = spawn_capture_server().await;
    let cfg = NotificationsConfig {
        slack: SlackConfig::default(),
        webhook: WebhookConfig {
            enabled: true,
            url: format!("http://{addr}/hook"),
        },
        email: Default::default(),
    };
    let notifiers = build_notifiers(&cfg).expect("notifiers");
    assert_eq!(notifiers.len(), 1);

    let event = make_event(AlertChannel::Webhook, Uuid::now_v7());
    notifiers[0].notify(&event).await.expect("notify");

    let captured = store.lock().clone();
    assert_eq!(captured.len(), 1);
    let body = &captured[0].body;
    assert_eq!(body["channel"], "webhook");
    assert_eq!(body["kind"], "down");
    assert_eq!(body["target_name"], "demo");
}

#[tokio::test]
async fn build_notifiers_rejects_enabled_slack_without_url() {
    let cfg = NotificationsConfig {
        slack: SlackConfig {
            enabled: true,
            webhook_url: String::new(),
        },
        webhook: WebhookConfig::default(),
        email: Default::default(),
    };
    assert!(build_notifiers(&cfg).is_err());
}

#[tokio::test]
async fn build_notifiers_rejects_enabled_webhook_without_url() {
    let cfg = NotificationsConfig {
        slack: SlackConfig::default(),
        webhook: WebhookConfig {
            enabled: true,
            url: String::new(),
        },
        email: Default::default(),
    };
    assert!(build_notifiers(&cfg).is_err());
}
