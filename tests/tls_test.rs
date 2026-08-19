mod common;

use axum::Router;
use axum::routing::get;
use uptimepage::domain::{CheckStatus, ExpectedStatus};
use uptimepage::worker::execute_http_check;
use url::Url;
use uuid::Uuid;

use crate::common::{
    default_http_check, spawn_self_signed_tls_router, spawn_truncated_chain_tls_router, test_client,
};

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
    // Per-phase timings populated for the breakdown chart: an HTTPS check
    // records a TLS-handshake phase (the bug in #31 left these always None).
    assert!(result.connect_ms.is_some(), "connect phase must be timed");
    assert!(result.tls_ms.is_some(), "tls handshake phase must be timed");
    assert!(result.ttfb_ms.is_some(), "ttfb must be timed");
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
    // webpki calls this an unknown issuer either way; the probe reads the leaf
    // back to say which shape it is.
    assert_eq!(
        err, "certificate self-signed",
        "expected self-signed reason, got {err}"
    );
}

/// A leaf installed without its intermediate. Browsers hide this by fetching
/// the missing certificate over AIA, so the operator sees a padlock.
#[tokio::test]
async fn verify_tls_true_names_a_truncated_chain() {
    let addr = spawn_truncated_chain_tls_router(router()).await;
    let clients = test_client();
    let url = Url::parse(&format!("https://localhost:{}/", addr.port())).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.verify_tls = true;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &clients).await;

    assert_eq!(result.status, CheckStatus::Error);
    let err = result.error.expect("error message");
    assert_eq!(
        err, "certificate chain incomplete",
        "expected truncated-chain reason, got {err}"
    );
}
