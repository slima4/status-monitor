//! Integration coverage for the user-profile columns added in the
//! pre-mortem hardening pass: `signup_org_id`, `onboarding_completed_at`,
//! `last_seen_at` (wired via session touch), plus the `POST
//! /api/v1/me/onboarding/complete` endpoint.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{build_test_app_with_pg, drop_test_db, fresh_test_db, open_test_pool, with_session};
use status_monitor::domain::UserId;
use tower::ServiceExt;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    fresh_test_db("user_profile").await
}

async fn seed_user_with_org(pool: &sqlx::PgPool, email: &str, slug: &str) -> (Uuid, Uuid) {
    let (user_id,): (Uuid,) =
        sqlx::query_as("INSERT INTO users (email, display_name) VALUES ($1, 'u') RETURNING id")
            .bind(email)
            .fetch_one(pool)
            .await
            .unwrap();
    let (org_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ($1, 'Test Org') RETURNING id",
    )
    .bind(slug)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'owner')")
        .bind(user_id)
        .bind(org_id)
        .execute(pool)
        .await
        .unwrap();
    (user_id, org_id)
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn complete_onboarding_endpoint_is_idempotent() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (user, _) = seed_user_with_org(&pool, "onb@example.test", "onb-org").await;
    let (router, _) = build_test_app_with_pg(pool.clone(), |_| {}).await;
    let router = with_session(router, UserId(user), None, None);

    let post = |router: axum::Router| async move {
        router
            .oneshot(
                Request::post("/api/v1/me/onboarding/complete")
                    .header("x-requested-with", "status-monitor")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    };
    assert_eq!(post(router.clone()).await.status(), StatusCode::NO_CONTENT);
    let (first,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT onboarding_completed_at FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    let first = first.expect("stamp set after first POST");
    assert!(first > Utc::now() - chrono::Duration::seconds(5));

    assert_eq!(post(router.clone()).await.status(), StatusCode::NO_CONTENT);
    let (second,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT onboarding_completed_at FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(second, Some(first), "second POST must not move the stamp");

    pool.close().await;
    drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn onboarding_page_redirects_when_completed() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (user, _) = seed_user_with_org(&pool, "done@example.test", "done-org").await;
    sqlx::query("UPDATE users SET onboarding_completed_at = now() WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let (router, _) = build_test_app_with_pg(pool.clone(), |_| {}).await;
    let router = with_session(router, UserId(user), None, None);

    let resp = router
        .oneshot(Request::get("/onboarding/org").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").and_then(|v| v.to_str().ok()),
        Some("/")
    );

    pool.close().await;
    drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn signup_org_id_drives_onboarding_anchor_over_oldest_membership() {
    // Invariant: when both columns disagree (user has a `signup_org_id` row
    // *and* an older invited membership), the onboarding page anchors on the
    // signup org — the pre-mortem fix for the oldest-membership heuristic.
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (user, _) = seed_user_with_org(&pool, "anchor@example.test", "old-invite-org").await;

    let (signup_org,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ('signup-org', 'Signup Org') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'owner')")
        .bind(user)
        .bind(signup_org)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET signup_org_id = $1 WHERE id = $2")
        .bind(signup_org)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

    let (router, _) = build_test_app_with_pg(pool.clone(), |_| {}).await;
    let router = with_session(router, UserId(user), None, None);

    let resp = router
        .oneshot(Request::get("/onboarding/org").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        body.contains(&signup_org.to_string()),
        "onboarding page must anchor on signup_org_id, not the older invite. body: {}",
        &body[..body.len().min(400)]
    );

    pool.close().await;
    drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn get_signup_org_id_skips_soft_deleted_org() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (user, org) = seed_user_with_org(&pool, "tomb@example.test", "tomb-org").await;
    sqlx::query("UPDATE users SET signup_org_id = $1 WHERE id = $2")
        .bind(org)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE organizations SET deleted_at = now() WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();

    let resolved = status_monitor::storage::users::get_signup_org_id(&pool, UserId(user))
        .await
        .unwrap();
    assert!(
        resolved.is_none(),
        "soft-deleted signup org must not be returned as the anchor"
    );

    pool.close().await;
    drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn session_touch_bumps_users_last_seen_at() {
    use status_monitor::auth::session::{
        build_debounce_cache, hash_session_id, touch_last_used_debounced,
    };

    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (user, _) = seed_user_with_org(&pool, "ls@example.test", "ls-org").await;
    let token = "raw-cookie-value-test";
    let id_hash = hash_session_id(token);
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '1 day')",
    )
    .bind(&id_hash)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    let cache = build_debounce_cache();
    touch_last_used_debounced(&pool, &cache, &id_hash)
        .await
        .unwrap();

    let (seen,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT last_seen_at FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    let seen = seen.expect("last_seen_at bumped by session touch");
    assert!(
        seen > Utc::now() - chrono::Duration::seconds(5),
        "last_seen_at must be recent"
    );

    pool.close().await;
    drop_test_db(&name).await;
}
