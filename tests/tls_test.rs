mod common;

use axum::Router;
use axum::routing::get;
use status_monitor::domain::{CheckStatus, ExpectedStatus};
use status_monitor::worker::execute_http_check;
use url::Url;
use uuid::Uuid;

use crate::common::{default_http_check, spawn_self_signed_tls_router, test_client};

fn router() -> Router {
    Router::new().route("/", get(|| async { "ok" }))
}

#[tokio::test]
async fn verify_tls_false_accepts_self_signed() {
    let addr = spawn_self_signed_tls_router(router()).await;
    let clients = test_client();
    let url = Url::parse(&format!("https://localhost:{}/", addr.port())).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.verify_tls = false;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &clients).await;

    assert_eq!(
        result.status,
        CheckStatus::Up,
        "expected Up, got {:?} (error: {:?})",
        result.status,
        result.error
    );
    assert_eq!(result.response_code, Some(200));
}

#[tokio::test]
async fn verify_tls_true_rejects_self_signed() {
    let addr = spawn_self_signed_tls_router(router()).await;
    let clients = test_client();
    let url = Url::parse(&format!("https://localhost:{}/", addr.port())).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.verify_tls = true;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &clients).await;

    assert_eq!(result.status, CheckStatus::Error);
    let err = result.error.expect("error message");
    assert!(
        ["connect", "transport", "request"].contains(&err.as_str()),
        "expected TLS-class error, got {err}",
    );
}
