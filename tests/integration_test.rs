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

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

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

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

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

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

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

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Down);
}

#[tokio::test]
async fn http_check_connection_refused_is_error() {
    let client = test_client();
    let url = Url::parse("http://127.0.0.1:1/").unwrap();
    let check = default_http_check(url, ExpectedStatus::Exact(200));

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Error);
    assert!(result.error.is_some());
}

#[tokio::test]
async fn http_check_dns_failure_is_error() {
    let client = test_client_with_failing_dns();
    let url = Url::parse("http://nonexistent.invalid./").unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.timeout = Duration::from_millis(500);

    let started = std::time::Instant::now();
    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;
    let elapsed = started.elapsed();

    assert_eq!(result.status, CheckStatus::Error);
    assert!(result.error.is_some());
    assert!(result.response_code.is_none());
    assert!(
        elapsed < Duration::from_secs(1),
        "dns resolution should not escape request timeout: elapsed {elapsed:?}"
    );
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
    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;
    let elapsed = started.elapsed();

    assert_eq!(result.status, CheckStatus::Error);
    assert_eq!(result.error.as_deref(), Some("timeout"));
    assert!(
        elapsed < Duration::from_millis(500),
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

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;
    sink.write_batch(&[result]).await.unwrap();

    assert_eq!(sink.len(), 1);
    assert_eq!(sink.snapshot()[0].status, CheckStatus::Up);
}

// ── Redirect following (regression: apex domains 301 to www/https) ──────────

use axum::http::header::LOCATION;
use axum::response::IntoResponse;

fn moved(code: StatusCode, to: &'static str) -> impl IntoResponse {
    (code, [(LOCATION, to)])
}

#[tokio::test]
async fn http_check_follows_redirect_to_up() {
    let app = Router::new()
        .route(
            "/",
            get(|| async { moved(StatusCode::MOVED_PERMANENTLY, "/health") }),
        )
        .route("/health", get(|| async { "pong" }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.follow_redirects = true;
    check.max_redirects = 5;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Up);
    assert_eq!(result.response_code, Some(200));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn http_check_redirect_not_followed_is_down() {
    let app = Router::new().route(
        "/",
        get(|| async { moved(StatusCode::MOVED_PERMANENTLY, "/health") }),
    );
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/")).unwrap();
    // default_http_check leaves follow_redirects = false.
    let check = default_http_check(url, ExpectedStatus::Exact(200));

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Down);
    assert_eq!(result.response_code, Some(301));
    assert_eq!(result.error.as_deref(), Some("unexpected status 301"));
}

#[tokio::test]
async fn http_check_follows_redirect_chain_within_budget() {
    let app = Router::new()
        .route("/a", get(|| async { moved(StatusCode::FOUND, "/b") }))
        .route(
            "/b",
            get(|| async { moved(StatusCode::SEE_OTHER, "/health") }),
        )
        .route("/health", get(|| async { "pong" }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/a")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.follow_redirects = true;
    check.max_redirects = 5;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Up);
    assert_eq!(result.response_code, Some(200));
}

#[tokio::test]
async fn http_check_redirect_loop_hits_budget() {
    let app = Router::new().route("/loop", get(|| async { moved(StatusCode::FOUND, "/loop") }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/loop")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.follow_redirects = true;
    check.max_redirects = 2;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Error);
    assert_eq!(result.error.as_deref(), Some("too many redirects"));
}

#[tokio::test]
async fn http_check_307_preserves_method_and_body() {
    use axum::routing::post;

    let app = Router::new()
        .route(
            "/start",
            post(|| async { moved(StatusCode::TEMPORARY_REDIRECT, "/echo") }),
        )
        // GET would 405 here — proving the hop stayed POST.
        .route(
            "/echo",
            post(|body: String| async move { format!("echo:{body}") }),
        );
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/start")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.method = status_monitor::domain::HttpMethod::Post;
    check.body = Some("ping".to_string());
    check.expected_body_contains = Some("echo:ping".to_string());
    check.follow_redirects = true;
    check.max_redirects = 5;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(result.status, CheckStatus::Up, "307 must keep POST + body");
    assert_eq!(result.response_code, Some(200));
}

#[tokio::test]
async fn http_check_strips_credentials_cross_origin() {
    use axum::http::HeaderMap;

    // Foreign origin: 200 only when NO Authorization header arrived.
    let foreign = Router::new().route(
        "/secure",
        get(|headers: HeaderMap| async move {
            if headers.contains_key(axum::http::header::AUTHORIZATION) {
                (StatusCode::UNAUTHORIZED, "leaked")
            } else {
                (StatusCode::OK, "clean")
            }
        }),
    );
    let foreign_addr = spawn_router(foreign).await;

    let origin = Router::new().route(
        "/",
        get(move || async move {
            moved(
                StatusCode::FOUND,
                Box::leak(format!("http://{foreign_addr}/secure").into_boxed_str()),
            )
        }),
    );
    let origin_addr = spawn_router(origin).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{origin_addr}/")).unwrap();
    let mut check = default_http_check(url, ExpectedStatus::Exact(200));
    check.bearer_token = Some("super-secret".to_string());
    check.expected_body_contains = Some("clean".to_string());
    check.follow_redirects = true;
    check.max_redirects = 5;

    let result = execute_http_check(Uuid::now_v7(), uuid::Uuid::nil(), &check, &client).await;

    assert_eq!(
        result.status,
        CheckStatus::Up,
        "bearer token must not cross to a foreign origin"
    );
}

/// Regression for the tenant-isolation write bug: `worker::execute` (and the
/// per-protocol check fns it dispatches to) must stamp the *passed* org_id
/// onto the produced `CheckResult`. The live-CH `tenant_isolation_test` also
/// covers this but is `#[ignore]`d — this is the fast, CI-visible guard. A
/// distinct non-nil org is used so it can't pass by coincidence with a
/// defaulted/zeroed field.
#[tokio::test]
async fn execute_stamps_passed_org_id_on_result() {
    let app = Router::new().route("/health", get(|| async { "pong" }));
    let addr = spawn_router(app).await;

    let client = test_client();
    let url = Url::parse(&format!("http://{addr}/health")).unwrap();
    let check = default_http_check(url, ExpectedStatus::Exact(200));
    let spec = status_monitor::domain::CheckSpec::Http(check);

    let target_id = Uuid::now_v7();
    let org_id = Uuid::now_v7();
    let domain_expiry = common::test_domain_expiry_runtime();
    let deps = status_monitor::worker::WorkerDeps {
        http: &client,
        domain_expiry: &domain_expiry,
    };
    let result = status_monitor::worker::execute(target_id, org_id, &spec, &deps).await;

    assert_eq!(result.status, CheckStatus::Up);
    assert_eq!(
        result.org_id, org_id,
        "worker::execute must thread the passed org_id onto the CheckResult"
    );
    assert_eq!(result.target_id, target_id);
}
