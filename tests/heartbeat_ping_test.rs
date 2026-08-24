//! Integration contract for the inbound heartbeat surface. InMemory-backed, no
//! DB. Requests carry no session or CSRF header, proving bare curl/cron works.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uptimepage::domain::OrgId;
use uuid::Uuid;

use common::build_test_app_state;

#[tokio::test]
async fn ping_accepts_get_and_post_and_advances_the_anchor() {
    let state = build_test_app_state(|_| {});
    let org = OrgId(Uuid::new_v4());
    let target_id = Uuid::new_v4();
    let hb = state
        .heartbeat_store
        .ensure(org, target_id)
        .await
        .unwrap()
        .unwrap();
    let token = hb.token.expect("in-memory store returns the raw token");
    let runtime = state.heartbeat_runtime.clone();
    let router = uptimepage::build_app_router(state, CancellationToken::new());

    assert!(runtime.state(target_id).is_none());
    for method in [Method::GET, Method::POST] {
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(format!("/ping/{token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{method} must count");
    }
    let anchor = runtime
        .state(target_id)
        .expect("ping set the state")
        .success_at;
    assert!(
        chrono::Utc::now()
            .signed_duration_since(anchor)
            .num_seconds()
            < 5
    );
}

/// `/start` opens a run without holding the monitor up, `/fail` and a nonzero
/// exit fail it outright, a later success clears that.
#[tokio::test]
async fn signals_report_the_run_the_bare_url_cannot() {
    let state = build_test_app_state(|_| {});
    let org = OrgId(Uuid::new_v4());
    let target_id = Uuid::new_v4();
    let token = state
        .heartbeat_store
        .ensure(org, target_id)
        .await
        .unwrap()
        .unwrap()
        .token
        .unwrap();
    let runtime = state.heartbeat_runtime.clone();
    let router = uptimepage::build_app_router(state, CancellationToken::new());

    let send = |path: String, body: &'static str| {
        let router = router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(path)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(
        send(format!("/ping/{token}/start"), "").await,
        StatusCode::OK
    );
    let state_after_start = runtime.state(target_id).expect("start recorded");
    assert!(state_after_start.run_open_since().is_some());
    assert!(
        state_after_start.failing().is_none(),
        "a start is not a verdict"
    );

    assert_eq!(
        send(format!("/ping/{token}/137"), "OOM killed").await,
        StatusCode::OK
    );
    let failed = runtime.state(target_id).expect("fail recorded");
    assert_eq!(failed.failing().and_then(|f| f.exit_code), Some(137));
    assert!(failed.run_open_since().is_none(), "the fail closed the run");

    assert_eq!(send(format!("/ping/{token}/0"), "").await, StatusCode::OK);
    assert!(
        runtime.state(target_id).unwrap().failing().is_none(),
        "exit 0 is a success and clears the failure"
    );

    // A word we don't know must not be mistaken for a success.
    assert_eq!(
        send(format!("/ping/{token}/probably"), "").await,
        StatusCode::NOT_FOUND
    );
}

/// The pending gate's exit: the first ping through the real route is what
/// wires a monitor up, whatever signal it carries.
#[tokio::test]
async fn the_first_ping_through_the_route_wires_the_monitor_up() {
    for signal in ["", "/start", "/fail", "/1", "/0"] {
        let state = build_test_app_state(|_| {});
        let org = OrgId(Uuid::new_v4());
        let target_id = Uuid::new_v4();
        let token = state
            .heartbeat_store
            .ensure(org, target_id)
            .await
            .unwrap()
            .unwrap()
            .token
            .unwrap();
        let store = state.heartbeat_store.clone();
        let router = uptimepage::build_app_router(state, CancellationToken::new());

        assert!(
            store
                .get(org, target_id)
                .await
                .unwrap()
                .unwrap()
                .first_ping_at
                .is_none(),
            "minting a token is not the job speaking"
        );

        let res = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/ping/{token}{signal}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "signal {signal:?}");
        assert!(
            store
                .get(org, target_id)
                .await
                .unwrap()
                .unwrap()
                .first_ping_at
                .is_some(),
            "signal {signal:?} left the monitor pending"
        );
    }
}

/// A rejected ping must not wire anything up: an unknown token resolves to no
/// row, so there is nothing to stamp.
#[tokio::test]
async fn a_refused_ping_wires_nothing_up() {
    let state = build_test_app_state(|_| {});
    let org = OrgId(Uuid::new_v4());
    let target_id = Uuid::new_v4();
    state.heartbeat_store.ensure(org, target_id).await.unwrap();
    let store = state.heartbeat_store.clone();
    let router = uptimepage::build_app_router(state, CancellationToken::new());

    for uri in ["/ping/not-a-token", "/ping/not-a-token/start"] {
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
    }
    assert!(
        store
            .get(org, target_id)
            .await
            .unwrap()
            .unwrap()
            .first_ping_at
            .is_none()
    );
}

#[tokio::test]
async fn unknown_token_is_a_uniform_404() {
    let state = build_test_app_state(|_| {});
    let router = uptimepage::build_app_router(state, CancellationToken::new());
    let res = router
        .oneshot(
            Request::builder()
                .uri("/ping/not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ping_flood_trips_the_per_monitor_rate_limit() {
    let state = build_test_app_state(|_| {});
    let org = OrgId(Uuid::new_v4());
    let target_id = Uuid::new_v4();
    let token = state
        .heartbeat_store
        .ensure(org, target_id)
        .await
        .unwrap()
        .unwrap()
        .token
        .unwrap();
    let router = uptimepage::build_app_router(state, CancellationToken::new());

    let mut saw_429 = false;
    for _ in 0..40 {
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/ping/{token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
        assert_eq!(res.status(), StatusCode::OK);
    }
    assert!(saw_429, "a tight ping loop must eventually 429");
}
