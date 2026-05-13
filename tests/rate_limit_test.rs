mod common;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::build_test_app;
use status_monitor::config::RateLimitConfig;
use tower::ServiceExt;

fn app_with_rate_limit(rl: RateLimitConfig) -> axum::Router {
    build_test_app(|cfg| cfg.api.rate_limit = rl)
}

fn request_with_peer(peer: SocketAddr) -> Request<Body> {
    let mut r = Request::get("/api/v1/targets").body(Body::empty()).unwrap();
    r.extensions_mut().insert(ConnectInfo(peer));
    r
}

#[tokio::test]
async fn rate_limit_returns_429_when_burst_exceeded() {
    let app = app_with_rate_limit(RateLimitConfig {
        enabled: true,
        per_second: 1,
        burst: 2,
    });
    let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let mut statuses = Vec::with_capacity(10);
    for _ in 0..10 {
        let resp = app.clone().oneshot(request_with_peer(peer)).await.unwrap();
        statuses.push(resp.status());
    }
    let too_many = statuses
        .iter()
        .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert!(
        too_many >= 5,
        "expected at least 5 429s under burst, got {statuses:?}"
    );
}

#[tokio::test]
async fn rate_limit_disabled_allows_burst() {
    let app = app_with_rate_limit(RateLimitConfig {
        enabled: false,
        per_second: 1,
        burst: 1,
    });
    let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    for _ in 0..20 {
        let resp = app.clone().oneshot(request_with_peer(peer)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn rate_limit_buckets_per_peer_ip() {
    let app = app_with_rate_limit(RateLimitConfig {
        enabled: true,
        per_second: 1,
        burst: 1,
    });
    for n in 1..=5 {
        let peer: SocketAddr = format!("10.0.0.{n}:12345").parse().unwrap();
        let resp = app.clone().oneshot(request_with_peer(peer)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "peer {peer} blocked");
    }
}
