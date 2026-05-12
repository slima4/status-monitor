mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use status_monitor::domain::{CheckStatus, ExpectedStatus};
use status_monitor::storage::{InMemorySink, ResultSink};
use status_monitor::worker::execute_http_check;
use url::Url;
use uuid::Uuid;

use crate::common::{default_http_check, spawn_router, test_client, test_client_with_failing_dns};

#[tokio::test]
async fn http_check_returns_up_on_200() {
    let app = Router::new().route("/health", get(|| async { "pong" }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/health")).unwrap();
    let check = default_http_check(url, ExpectedStatus::Exact(200));

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Up);
    assert_eq!(result.response_code, Some(200));
    assert_eq!(result.response_size, Some(4));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn http_check_returns_down_on_unexpected_status() {
    let app = Router::new().route(
        "/broken",
        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
    );
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/broken")).unwrap();
    let check = default_http_check(url, ExpectedStatus::Exact(200));

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Down);
    assert_eq!(result.response_code, Some(500));
    assert!(result.error.is_some());
}

#[tokio::test]
async fn http_check_status_range_matches() {
    let app = Router::new().route("/", get(|| async { (StatusCode::ACCEPTED, "") }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let check = default_http_check(url, ExpectedStatus::Range { min: 200, max: 299 });

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Up);
    assert_eq!(result.response_code, Some(202));
}

#[tokio::test]
async fn http_check_body_match_failure_is_down() {
    let app = Router::new().route("/", get(|| async { "hello world" }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.expected_body_contains = Some("goodbye".into());

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Down);
}

#[tokio::test]
async fn http_check_connection_refused_is_error() {
    let client = test_client();
    let url = Url::parse("http://127.0.0.1:1/").unwrap();
    let check = default_http_check(url, ExpectedStatus::Exact(200));

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Error);
    assert!(result.error.is_some());
}

#[tokio::test]
async fn http_check_dns_failure_is_error() {
    let client = test_client_with_failing_dns();
    let url = Url::parse("http://nonexistent.invalid./").unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.timeout = Duration::from_millis(500);

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Error);
    assert!(result.error.is_some());
    assert!(result.response_code.is_none());
}

#[tokio::test]
async fn http_check_total_timeout_is_error() {
    let app = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            "late"
        }),
    );
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/slow")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.timeout = Duration::from_millis(150);

    let started = std::time::Instant::now();
    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;
    let elapsed = started.elapsed();

    assert_eq!(result.status, CheckStatus::Error);
    assert_eq!(result.error.as_deref(), Some("timeout"));
    assert!(
        elapsed < Duration::from_secs(1),
        "timeout not enforced: elapsed {elapsed:?}"
    );
}

#[tokio::test]
async fn in_memory_sink_collects_results() {
    let app = Router::new().route("/", get(|| async { "ok" }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let sink = Arc::new(InMemorySink::new());
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let check = default_http_check(url, ExpectedStatus::Exact(200));

    let result = execute_http_check(Uuid::now_v7(), &check, &client).await;
    sink.write_batch(&[result]).await.unwrap();

    assert_eq!(sink.len(), 1);
    assert_eq!(sink.snapshot()[0].status, CheckStatus::Up);
}
