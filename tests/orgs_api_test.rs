//! HTTP-level tests for the org-management endpoints. They walk every route
//! from `Router::oneshot` through the real handlers, so anything that breaks
//! the request shape (extractor wiring, status codes, JSON contract) surfaces
//! here rather than only at the storage layer.
//!
//! Live-PG ignored: the orgs routes need a Postgres pool. Sessions are stamped
//! onto requests via a `from_fn` layer that inserts a constructed `Session`
//! into the extensions — the real auth backend will replace it; until then
//! this is the only way to drive authenticated routes.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{body_json, build_test_app_with_pg, session_layer};
use serde_json::{Value, json};
use sqlx::PgPool;
use status_monitor::domain::UserId;
use status_monitor::web::{Session, User};
use tower::ServiceExt;
use uuid::Uuid;

async fn make_user(pool: &PgPool) -> UserId {
    let email = format!("apitest-{}@example.test", Uuid::now_v7());
    let (id,): (Uuid,) = sqlx::query_as(r#"INSERT INTO users (email) VALUES ($1) RETURNING id"#)
        .bind(&email)
        .fetch_one(pool)
        .await
        .expect("seed user");
    UserId(id)
}

fn cleanup_user(pool: PgPool, user: UserId) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user.0)
            .execute(&pool)
            .await;
    })
}

fn app_with_session(router: Router, user: UserId) -> Router {
    router.layer(session_layer(Session {
        user: Some(User {
            id: user,
            email: format!("u-{}@example.test", user.0),
        }),
        active_org_id: None,
    }))
}

fn unique_slug(prefix: &str) -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &id[id.len() - 6..])
}

async fn read_value(resp: axum::http::Response<Body>) -> Value {
    body_json(resp).await
}

#[tokio::test]
#[ignore]
async fn unauthenticated_request_returns_401() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (router, _) = build_test_app_with_pg(pool, |cfg| cfg.tenancy.enabled = true).await;
    // No session layer attached — Session::user is None → CurrentUser → 401.
    let resp = router
        .oneshot(Request::get("/api/v1/orgs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore]
async fn create_then_list_then_delete_round_trip() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |cfg| cfg.tenancy.enabled = true).await;
    let router = app_with_session(router, user);

    let slug = unique_slug("api");
    let create = router
        .clone()
        .oneshot(
            Request::post("/api/v1/orgs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "slug": slug, "name": "API Co" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let body = read_value(create).await;
    let org_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["slug"], slug);
    assert_eq!(body["role"], "owner");

    // List my orgs — must include the new one.
    let list = router
        .clone()
        .oneshot(Request::get("/api/v1/me/orgs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let arr = read_value(list).await;
    assert!(arr.as_array().unwrap().iter().any(|o| o["id"] == org_id));

    // Soft-delete (owner-only).
    let del = router
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/orgs/{org_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);

    // Now hidden from /orgs and surfaced in /me/deleted-orgs.
    let after = router
        .clone()
        .oneshot(Request::get("/api/v1/me/orgs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let arr = read_value(after).await;
    assert!(arr.as_array().unwrap().iter().all(|o| o["id"] != org_id));

    let deleted = router
        .clone()
        .oneshot(
            Request::get("/api/v1/me/deleted-orgs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let arr = read_value(deleted).await;
    assert!(arr.as_array().unwrap().iter().any(|o| o["id"] == org_id));

    // Restore — must succeed because caller is the deleter and we're inside grace.
    let restore = router
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/orgs/{org_id}/restore"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restore.status(), StatusCode::OK);

    cleanup_user(pool, user).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn create_org_with_taken_slug_returns_409() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |cfg| cfg.tenancy.enabled = true).await;
    let router = app_with_session(router, user);

    let slug = unique_slug("dup");
    let first = router
        .clone()
        .oneshot(
            Request::post("/api/v1/orgs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "slug": slug, "name": "A" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = router
        .clone()
        .oneshot(
            Request::post("/api/v1/orgs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "slug": slug, "name": "B" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let body = read_value(second).await;
    assert_eq!(body["error"]["code"], "SLUG_TAKEN");

    cleanup_user(pool, user).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn create_org_with_reserved_slug_returns_400() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |cfg| cfg.tenancy.enabled = true).await;
    let router = app_with_session(router, user);

    let resp = router
        .oneshot(
            Request::post("/api/v1/orgs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "slug": "admin", "name": "X" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_value(resp).await;
    assert_eq!(body["error"]["code"], "SLUG_INVALID");

    cleanup_user(pool, user).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn check_slug_endpoint_returns_availability() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |cfg| cfg.tenancy.enabled = true).await;
    let router = app_with_session(router, user);

    let slug = unique_slug("chk");
    let resp = router
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/orgs/check-slug?slug={slug}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(read_value(resp).await["available"], true);

    // Reserved slug: not available, reason populated.
    let resp = router
        .clone()
        .oneshot(
            Request::get("/api/v1/orgs/check-slug?slug=admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_value(resp).await;
    assert_eq!(body["available"], false);
    assert!(body["reason"].as_str().unwrap().contains("reserved"));

    cleanup_user(pool, user).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn non_owner_cannot_delete_or_update() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let owner = make_user(&pool).await;
    let other = make_user(&pool).await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |cfg| cfg.tenancy.enabled = true).await;
    let owner_router = app_with_session(router.clone(), owner);
    let other_router = app_with_session(router, other);

    let slug = unique_slug("priv");
    let create = owner_router
        .clone()
        .oneshot(
            Request::post("/api/v1/orgs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "slug": slug, "name": "P" })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let body = read_value(create).await;
    let org_id = body["id"].as_str().unwrap().to_string();

    // Non-member sees 404 (cloak).
    let del = other_router
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/orgs/{org_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(&[owner.0, other.0][..])
        .execute(&pool)
        .await
        .unwrap();
}
