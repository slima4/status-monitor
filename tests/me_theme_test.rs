//! Integration coverage for `/api/v1/me/theme`. Asserts the GET / PATCH pair
//! persists to `users.theme`, emits the `sm_theme` cookie, and rejects unknown
//! variants at deserialise time.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo +1.95 nextest run --test me_theme_test --run-ignored all

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    body_json, build_test_app_with_pg, drop_test_db, fresh_test_db, json_request, open_test_pool,
    with_session,
};
use status_monitor::domain::UserId;
use tower::ServiceExt;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    fresh_test_db("me_theme").await
}

async fn seed_user(pool: &sqlx::PgPool, email: &str) -> Uuid {
    let (id,): (Uuid,) =
        sqlx::query_as("INSERT INTO users (email, display_name) VALUES ($1, 'u') RETURNING id")
            .bind(email)
            .fetch_one(pool)
            .await
            .unwrap();
    id
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn me_theme_get_patch_round_trip() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool, "themer@example.test").await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |_| {}).await;
    let router = with_session(router, UserId(user), None, None);

    let resp = router
        .clone()
        .oneshot(
            Request::get("/api/v1/me/theme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["theme"], "default");

    let resp = router
        .clone()
        .oneshot({
            let mut req = json_request(
                "PATCH",
                "/api/v1/me/theme",
                serde_json::json!({"theme": "terminal"}),
            );
            req.headers_mut()
                .insert("x-requested-with", "status-monitor".parse().unwrap());
            req
        })
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookies: Vec<&str> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert!(
        set_cookies
            .iter()
            .any(|c| c.starts_with("sm_theme=terminal")),
        "Set-Cookie sm_theme=terminal missing from {set_cookies:?}"
    );
    let body = body_json(resp).await;
    assert_eq!(body["theme"], "terminal");

    let (stored,): (String,) = sqlx::query_as("SELECT theme FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "terminal");

    pool.close().await;
    drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn me_theme_patch_rejects_unknown_variant() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool, "themer-bad@example.test").await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |_| {}).await;
    let router = with_session(router, UserId(user), None, None);

    let resp = router
        .oneshot({
            let mut req = json_request(
                "PATCH",
                "/api/v1/me/theme",
                serde_json::json!({"theme": "garbage"}),
            );
            req.headers_mut()
                .insert("x-requested-with", "status-monitor".parse().unwrap());
            req
        })
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let (stored,): (String,) = sqlx::query_as("SELECT theme FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "default", "DB must be untouched on bad input");

    pool.close().await;
    drop_test_db(&name).await;
}
