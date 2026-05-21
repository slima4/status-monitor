//! Live-PG tests for the user hard-purge half of the soft-delete worker.
//! Skipped by default; run under `--run-ignored` (or `--include-ignored`)
//! once `DATABASE_URL` is set. No ClickHouse needed — the user purge is
//! pure Postgres. Tests seed their own users (unique emails) and assert on
//! their own rows; the shared dev DB and the per-call `PURGE_BATCH_LIMIT`
//! mean the "purged" cases backdate far enough to sort first.

mod common;

use common::{make_user, unique_slug};
use status_monitor::domain::{OrgId, UserId};
use status_monitor::jobs::purge_deleted::purge_users_past_grace;
use status_monitor::storage::create_org_with_owner;
use uuid::Uuid;

const GRACE: u32 = 30;

/// Backdate `deleted_at` so the grace check fires without sleeping.
async fn soft_delete_backdated(pool: &sqlx::PgPool, user: UserId, days_ago: i64) {
    sqlx::query("UPDATE users SET deleted_at = now() - ($2::int * INTERVAL '1 day') WHERE id = $1")
        .bind(user.0)
        .bind(days_ago)
        .execute(pool)
        .await
        .expect("backdate user delete");
}

/// Insert a recovery token. `expires_in_days` may be negative (already
/// expired); `used` sets `used_at` (a used token never blocks purge and is
/// excluded from the one-active-per-user partial unique index).
async fn insert_recovery(
    pool: &sqlx::PgPool,
    user: UserId,
    expires_in_days: i64,
    used: bool,
) -> Result<(), sqlx::Error> {
    let prefix = format!("rec-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO user_recovery_tokens \
            (user_id, token_hash, token_prefix, expires_at, used_at) \
         VALUES ($1, 'argon2-hash', $2, \
                 now() + ($3::int * INTERVAL '1 day'), \
                 CASE WHEN $4 THEN now() ELSE NULL END)",
    )
    .bind(user.0)
    .bind(&prefix)
    .bind(expires_in_days)
    .bind(used)
    .execute(pool)
    .await
    .map(|_| ())
}

async fn user_exists(pool: &sqlx::PgPool, user: UserId) -> bool {
    let (e,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
        .bind(user.0)
        .fetch_one(pool)
        .await
        .unwrap();
    e
}

async fn count(pool: &sqlx::PgPool, sql: &str, user: UserId) -> i64 {
    let (n,): (i64,) = sqlx::query_as(sql)
        .bind(user.0)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

async fn cleanup_user(pool: &sqlx::PgPool, user: UserId) {
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(pool)
        .await;
}

async fn cleanup_org(pool: &sqlx::PgPool, org: OrgId) {
    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org.0)
        .execute(pool)
        .await;
}

/// T2 — the partial unique index allows at most one active (unused)
/// recovery token per user; a second deletion attempt must collide.
#[tokio::test]
#[ignore]
async fn second_active_recovery_token_violates_unique() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "purge-user").await;

    insert_recovery(&pool, user, 1, false)
        .await
        .expect("first active token inserts");
    let err = insert_recovery(&pool, user, 1, false)
        .await
        .expect_err("second active token must violate the partial unique index");
    assert!(
        err.as_database_error()
            .is_some_and(|e| e.is_unique_violation()),
        "expected a unique violation, got: {err}"
    );

    cleanup_user(&pool, user).await;
}

/// T3 — a user still inside the grace window is never purged.
#[tokio::test]
#[ignore]
async fn user_within_grace_is_kept() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "purge-user").await;
    soft_delete_backdated(&pool, user, 29).await;

    purge_users_past_grace(&pool, GRACE).await.unwrap();

    assert!(
        user_exists(&pool, user).await,
        "user within grace must survive the purge"
    );
    cleanup_user(&pool, user).await;
}

/// T4 — a user past grace with no live recovery token is hard-deleted.
/// (The recovery endpoint then returns 410 because the row is gone — that
/// is endpoint-level and asserted once the endpoint lands.)
#[tokio::test]
#[ignore]
async fn user_past_grace_is_purged() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "purge-user").await;
    soft_delete_backdated(&pool, user, 365).await;

    // Count is racy on the shared DB (a parallel test's batch call may grab
    // this user first); the row-gone state is deterministic either way.
    purge_users_past_grace(&pool, GRACE).await.unwrap();
    assert!(
        !user_exists(&pool, user).await,
        "past-grace user row must be physically gone"
    );
}

/// T5 — a user who recovered (deleted_at cleared, token marked used) is
/// not eligible: the `deleted_at IS NOT NULL` predicate excludes them.
#[tokio::test]
#[ignore]
async fn recovered_user_is_not_purged() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    // "Recovered" = deleted_at cleared and the recovery token marked used.
    // deleted_at IS NULL throughout, so no concurrent purge tick can ever
    // consider this user eligible (testing the predicate, not a NOT-NULL
    // window that another test's batch call could race into).
    let user = make_user(&pool, "purge-user").await;
    insert_recovery(&pool, user, -1, true).await.unwrap(); // expired + used

    purge_users_past_grace(&pool, GRACE).await.unwrap();

    assert!(
        user_exists(&pool, user).await,
        "a recovered (un-deleted) user must never be purged"
    );
    cleanup_user(&pool, user).await;
}

/// T6 — two delete/recover cycles. A used token (cycle 1) and an expired
/// token (cycle 2) neither block the purge; the user and both tokens go.
#[tokio::test]
#[ignore]
async fn two_delete_recover_cycles_purge_correctly() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "purge-user").await;
    // Cycle 1: token issued then used (recovered).
    insert_recovery(&pool, user, -36, true).await.unwrap();
    // Cycle 2: soft-deleted again, token issued but now expired and unused.
    soft_delete_backdated(&pool, user, 365).await;
    insert_recovery(&pool, user, -1, false).await.unwrap();

    purge_users_past_grace(&pool, GRACE).await.unwrap();
    assert!(!user_exists(&pool, user).await, "user gone after cycle 2");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM user_recovery_tokens WHERE user_id = $1",
            user
        )
        .await,
        0,
        "both recovery tokens cascade-deleted with the user"
    );
}

/// T7 — the `users` FK cascade leaves no orphan rows in the dependent
/// tables after a hard purge.
#[tokio::test]
#[ignore]
async fn fk_cascade_leaves_no_orphans() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "purge-user").await;
    let org = create_org_with_owner(&pool, user, &unique_slug("casc"), "n", 3)
        .await
        .unwrap()
        .unwrap();

    sqlx::query(
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id) \
         VALUES ($1, 'github', $2)",
    )
    .bind(user.0)
    .bind(format!("gh-{}", Uuid::new_v4().simple()))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_tokens (user_id, name, token_hash, token_prefix) \
         VALUES ($1, 'k', 'h', $2)",
    )
    .bind(user.0)
    .bind(format!("tok-{}", Uuid::new_v4().simple()))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at) \
         VALUES ($1, $2, now() + INTERVAL '1 day')",
    )
    .bind(format!("sess-hash-{}", Uuid::new_v4().simple()))
    .bind(user.0)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO invitations \
            (org_id, inviter_id, email, role, token_hash, token_prefix, expires_at) \
         VALUES ($1, $2, $3, 'member', 'h', $4, now() + INTERVAL '7 days')",
    )
    .bind(org.id.0)
    .bind(user.0)
    .bind(format!("invitee-{}@x.example", Uuid::new_v4().simple()))
    .bind(format!("inv-{}", Uuid::new_v4().simple()))
    .execute(&pool)
    .await
    .unwrap();
    // Used token: present (so the cascade has something to erase) but does
    // not block the purge.
    insert_recovery(&pool, user, 1, true).await.unwrap();

    soft_delete_backdated(&pool, user, 365).await;
    purge_users_past_grace(&pool, GRACE).await.unwrap();
    assert!(!user_exists(&pool, user).await);

    for (table, sql) in [
        (
            "memberships",
            "SELECT count(*) FROM memberships WHERE user_id = $1",
        ),
        (
            "oauth_identities",
            "SELECT count(*) FROM oauth_identities WHERE user_id = $1",
        ),
        (
            "api_tokens",
            "SELECT count(*) FROM api_tokens WHERE user_id = $1",
        ),
        (
            "sessions",
            "SELECT count(*) FROM sessions WHERE user_id = $1",
        ),
        (
            "invitations",
            "SELECT count(*) FROM invitations WHERE inviter_id = $1",
        ),
        (
            "user_recovery_tokens",
            "SELECT count(*) FROM user_recovery_tokens WHERE user_id = $1",
        ),
    ] {
        assert_eq!(
            count(&pool, sql, user).await,
            0,
            "{table} must have no rows for the purged user"
        );
    }

    cleanup_org(&pool, org.id).await;
}

/// T8 — audit rows survive the purge with the actor nulled
/// (`ON DELETE SET NULL`), so credential-stuffing / audit history is not
/// erased when a user is forgotten.
#[tokio::test]
#[ignore]
async fn audit_rows_survive_with_nulled_actor() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "purge-user").await;
    let org = create_org_with_owner(&pool, user, &unique_slug("aud"), "n", 3)
        .await
        .unwrap()
        .unwrap();

    let (la_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO login_attempts (user_id, method, success) \
         VALUES ($1, 'github_oauth', true) RETURNING id",
    )
    .bind(user.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (al_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO org_audit_log (org_id, actor_id, action) \
         VALUES ($1, $2, 'member.added') RETURNING id",
    )
    .bind(org.id.0)
    .bind(user.0)
    .fetch_one(&pool)
    .await
    .unwrap();

    soft_delete_backdated(&pool, user, 365).await;
    purge_users_past_grace(&pool, GRACE).await.unwrap();
    assert!(!user_exists(&pool, user).await);

    let (la_user,): (Option<Uuid>,) =
        sqlx::query_as("SELECT user_id FROM login_attempts WHERE id = $1")
            .bind(la_id)
            .fetch_one(&pool)
            .await
            .expect("login_attempts row must survive");
    assert!(
        la_user.is_none(),
        "login_attempts.user_id nulled, not deleted"
    );

    let (al_actor,): (Option<Uuid>,) =
        sqlx::query_as("SELECT actor_id FROM org_audit_log WHERE id = $1")
            .bind(al_id)
            .fetch_one(&pool)
            .await
            .expect("org_audit_log row must survive");
    assert!(
        al_actor.is_none(),
        "org_audit_log.actor_id nulled, not deleted"
    );

    let _ = sqlx::query("DELETE FROM login_attempts WHERE id = $1")
        .bind(la_id)
        .execute(&pool)
        .await;
    cleanup_org(&pool, org.id).await;
}
