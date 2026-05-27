//! Live-PG tests for GDPR data export, account deletion, and recovery.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo +1.95 nextest run --test account_gdpr_test --run-ignored all
//!
//! Each test gets its own freshly-created database (migrations applied) so the
//! deletion/recovery transactions run against the real schema in isolation.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    body_json, build_test_app_with_pg, drop_test_db, fresh_test_db, open_test_pool, with_session,
};
use serde_json::json;
use status_monitor::api::error::codes;
use status_monitor::auth::account;
use status_monitor::error::AppError;
use tower::ServiceExt;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    fresh_test_db("gdpr").await
}

async fn drop_pg(test_db: &str) {
    drop_test_db(test_db).await;
}

async fn open_pool(db_url: &str) -> sqlx::PgPool {
    open_test_pool(db_url).await
}

async fn seed_user(pool: &sqlx::PgPool, email: &str, name: &str) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, display_name, terms_version, privacy_version) \
         VALUES ($1, $2, 'v1', 'v1') RETURNING id",
    )
    .bind(email)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    id
}

async fn seed_org(pool: &sqlx::PgPool, slug: &str, owner: Uuid) -> Uuid {
    let (id,): (Uuid,) =
        sqlx::query_as("INSERT INTO organizations (slug, name) VALUES ($1, $2) RETURNING id")
            .bind(slug)
            .bind(slug)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'owner')")
        .bind(owner)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn add_member(pool: &sqlx::PgPool, org: Uuid, user: Uuid, role: &str) {
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, $3)")
        .bind(user)
        .bind(org)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}

async fn is_deleted(pool: &sqlx::PgPool, table: &str, id: Uuid) -> bool {
    let sql = format!("SELECT deleted_at IS NOT NULL FROM {table} WHERE id = $1");
    let (b,): (bool,) = sqlx::query_as(&sql).bind(id).fetch_one(pool).await.unwrap();
    b
}

async fn assert_user_deleted(pool: &sqlx::PgPool, user: Uuid, expected: bool) {
    assert_eq!(is_deleted(pool, "users", user).await, expected);
}

async fn assert_org_deleted(pool: &sqlx::PgPool, org: Uuid, expected: bool) {
    assert_eq!(is_deleted(pool, "organizations", org).await, expected);
}

// ---------------------------------------------------------------------------
// deletion blocking
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn deletion_blocked_when_solely_owning_org_with_other_members() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "owner@example.test", "Owner").await;
    let other = seed_user(&pool, "member@example.test", "Member").await;
    let org = seed_org(&pool, "shared", owner).await;
    add_member(&pool, org, other, "member").await;

    let err = account::request_deletion(&pool, status_monitor::domain::UserId(owner), 30)
        .await
        .expect_err("must block");
    match err {
        AppError::UnprocessableDetails { code, details, .. } => {
            assert_eq!(code, codes::OWNS_SHARED_ORGS);
            let orgs = details["orgs"].as_array().expect("orgs array");
            assert_eq!(orgs.len(), 1);
            assert_eq!(orgs[0]["slug"], "shared");
        }
        other => panic!("expected OWNS_SHARED_ORGS, got {other:?}"),
    }

    // Nothing was mutated — the user and org are untouched.
    assert_user_deleted(&pool, owner, false).await;
    assert_org_deleted(&pool, org, false).await;

    pool.close().await;
    drop_pg(&name).await;
}

// ---------------------------------------------------------------------------
// delete then recover round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn deletion_then_recovery_round_trip() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool, "solo@example.test", "Solo").await;
    let uid = status_monitor::domain::UserId(user);
    let org = seed_org(&pool, "solo-org", user).await;
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at) VALUES ($1, $2, now() + interval '1 day')",
    )
    .bind("sess-1-hash")
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_tokens (user_id, name, token_hash, token_prefix) \
         VALUES ($1, 't', 'h', 'p')",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    let outcome = account::request_deletion(&pool, uid, 30)
        .await
        .expect("deletion succeeds");
    assert_eq!(outcome.email, "solo@example.test");
    assert!(!outcome.recovery_token.is_empty());

    // User + solo org soft-deleted; sessions/tokens gone; owner membership
    // kept (so recovery can restore access); exactly one active recovery row.
    assert_user_deleted(&pool, user, true).await;
    assert_org_deleted(&pool, org, true).await;
    let (sessions,): (i64,) = sqlx::query_as("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let (tokens,): (i64,) = sqlx::query_as("SELECT count(*) FROM api_tokens WHERE user_id = $1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tokens, 0);
    let (memberships,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM memberships WHERE user_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(memberships, 1, "solo-org owner membership must be kept");
    let (recoveries,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM user_recovery_tokens WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(recoveries, 1);

    // A second deletion of the now-soft-deleted account is rejected cleanly.
    match account::request_deletion(&pool, uid, 30).await {
        Err(AppError::Conflict { code, .. }) => {
            assert_eq!(code, codes::ACCOUNT_ALREADY_DELETED);
        }
        other => panic!("expected ACCOUNT_ALREADY_DELETED, got {other:?}"),
    }

    // Recover with the emailed token: account + org restored, token burned.
    let recovered = account::recover(&pool, &outcome.recovery_token)
        .await
        .expect("recovery succeeds");
    assert_eq!(recovered.user_id, uid);
    assert_user_deleted(&pool, user, false).await;
    assert_org_deleted(&pool, org, false).await;
    let (used,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM user_recovery_tokens WHERE user_id = $1 AND used_at IS NOT NULL",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(used, 1, "recovery token must be burned");

    // The burned link can't be replayed.
    match account::recover(&pool, &outcome.recovery_token).await {
        Err(AppError::NotFound { code, .. }) => {
            assert_eq!(code, codes::ACCOUNT_RECOVERY_INVALID);
        }
        other => panic!("expected ACCOUNT_RECOVERY_INVALID on replay, got {other:?}"),
    }

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn recovery_with_unknown_token_is_404() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    match account::recover(&pool, "definitely-not-a-real-token").await {
        Err(AppError::NotFound { code, .. }) => assert_eq!(code, codes::ACCOUNT_RECOVERY_INVALID),
        other => panic!("expected ACCOUNT_RECOVERY_INVALID, got {other:?}"),
    }

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn expired_recovery_token_cannot_recover() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool, "late@example.test", "Late").await;
    let uid = status_monitor::domain::UserId(user);
    seed_org(&pool, "late-org", user).await;
    let outcome = account::request_deletion(&pool, uid, 30).await.unwrap();

    // Slide the recovery window into the past — the grace period elapsed.
    sqlx::query(
        "UPDATE user_recovery_tokens SET expires_at = now() - interval '1 hour' WHERE user_id = $1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    match account::recover(&pool, &outcome.recovery_token).await {
        Err(AppError::NotFound { code, .. }) => assert_eq!(code, codes::ACCOUNT_RECOVERY_INVALID),
        other => panic!("expected ACCOUNT_RECOVERY_INVALID after expiry, got {other:?}"),
    }

    pool.close().await;
    drop_pg(&name).await;
}

/// Defensive 410 branch: the token verifies but the user row is already gone
/// (the un-delete `RETURNING` finds nothing). Simulated by clearing
/// `deleted_at` out from under a still-live token.
#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn recovery_returns_410_when_row_already_restored() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool, "race@example.test", "Race").await;
    let uid = status_monitor::domain::UserId(user);
    seed_org(&pool, "race-org", user).await;
    let outcome = account::request_deletion(&pool, uid, 30).await.unwrap();

    // Token stays live + unused, but the user is no longer soft-deleted.
    sqlx::query("UPDATE users SET deleted_at = NULL WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

    match account::recover(&pool, &outcome.recovery_token).await {
        Err(AppError::Gone { code, .. }) => assert_eq!(code, codes::ACCOUNT_GONE),
        other => panic!("expected ACCOUNT_GONE, got {other:?}"),
    }

    pool.close().await;
    drop_pg(&name).await;
}

// ---------------------------------------------------------------------------
// data export: redaction + cross-user exclusion
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn data_export_redacts_credentials_and_excludes_other_emails() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "exporter@example.test", "Exporter").await;
    let other = seed_user(&pool, "coworker-secret@example.test", "Coworker").await;
    let org = seed_org(&pool, "export-org", owner).await;
    add_member(&pool, org, other, "member").await;

    // Target carrying credentials, stored at rest (no KEK in the test state →
    // plaintext-at-rest, the case redaction must also cover).
    let check_spec = json!({
        "type": "http",
        "url": "https://example.test/healthz",
        "method": "GET",
        "basic_auth": ["admin", "hunter2"],
        "bearer_token": "super-secret-token"
    });
    sqlx::query(
        "INSERT INTO targets (org_id, name, check_spec, interval_secs) \
         VALUES ($1, 'api', $2, 60)",
    )
    .bind(org)
    .bind(&check_spec)
    .execute(&pool)
    .await
    .unwrap();

    let (router, _) = build_test_app_with_pg(pool.clone(), |_cfg| {}).await;
    let router = with_session(router, status_monitor::domain::UserId(owner), None, None);

    let resp = router
        .oneshot(
            Request::get("/api/v1/me/data-export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let owned = &body["owned_orgs"];
    assert_eq!(owned.as_array().unwrap().len(), 1);
    let target = &owned[0]["targets"][0];
    assert_eq!(target["check"]["basic_auth"], json!("***"));
    assert_eq!(target["check"]["bearer_token"], json!("***"));

    // Co-member appears by name + role only.
    let members = owned[0]["members"].as_array().unwrap();
    assert!(members.iter().any(|m| m["display_name"] == "Coworker"));
    for m in members {
        assert!(m.get("email").is_none(), "member rows must not carry email");
    }

    // The raw secrets and the other user's email must not appear anywhere in
    // the serialized export.
    let dump = serde_json::to_string(&body).unwrap();
    assert!(!dump.contains("hunter2"), "basic_auth secret leaked");
    assert!(!dump.contains("super-secret-token"), "bearer token leaked");
    assert!(
        !dump.contains("coworker-secret@example.test"),
        "third-party email leaked"
    );

    pool.close().await;
    drop_pg(&name).await;
}
