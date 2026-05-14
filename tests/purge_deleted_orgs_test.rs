//! Live-PG + live-CH tests for the soft-delete purge worker. Skipped by
//! default; runs under `--include-ignored` once `DATABASE_URL` and
//! `CLICKHOUSE_URL` are set. The tests seed their own orgs/users + write
//! check_results rows tagged with the org id, then exercise the cascade and
//! the queue-drain.

mod common;

use status_monitor::domain::{OrgId, UserId};
use status_monitor::jobs::purge_deleted_orgs::{drain_clickhouse_purge_queue, purge_tick};
use status_monitor::storage::{create_org_with_owner, soft_delete_org};
use uuid::Uuid;

async fn make_user(pool: &sqlx::PgPool) -> UserId {
    let email = format!("u-{}@test.example", Uuid::now_v7());
    let (id,): (Uuid,) = sqlx::query_as(r#"INSERT INTO users (email) VALUES ($1) RETURNING id"#)
        .bind(&email)
        .fetch_one(pool)
        .await
        .expect("insert user");
    UserId(id)
}

fn unique_slug(prefix: &str) -> String {
    let id = Uuid::new_v4().simple().to_string();
    let suffix = &id[id.len() - 6..];
    format!("{prefix}-{suffix}")
}

/// Backdate `deleted_at` so the row is past the grace window without sleeping.
async fn backdate_delete(pool: &sqlx::PgPool, org: OrgId, days_ago: i32) {
    sqlx::query(
        "UPDATE organizations SET deleted_at = now() - ($2::int * INTERVAL '1 day') WHERE id = $1",
    )
    .bind(org.0)
    .bind(days_ago)
    .execute(pool)
    .await
    .expect("backdate delete");
}

#[tokio::test]
#[ignore]
async fn grace_window_blocks_purge() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let org = create_org_with_owner(&pool, user, &unique_slug("grace"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    soft_delete_org(&pool, org.id, user).await.unwrap();

    // Inside grace (deleted ~now), tick should not cascade.
    let stats = purge_tick(&pool, &ch, 30).await.unwrap();
    assert_eq!(stats.cascaded, 0);
    // Row still exists.
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM organizations WHERE id = $1)")
            .bind(org.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(exists, "org should still exist inside grace window");

    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn past_grace_cascades_and_enqueues_ch() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let org = create_org_with_owner(&pool, user, &unique_slug("past"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    soft_delete_org(&pool, org.id, user).await.unwrap();
    backdate_delete(&pool, org.id, 40).await;

    let stats = purge_tick(&pool, &ch, 30).await.unwrap();
    assert!(stats.cascaded >= 1, "expected at least one cascade");

    // PG row gone via cascade.
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM organizations WHERE id = $1)")
            .bind(org.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!exists, "org should be hard-deleted");

    // Queue row exists; it either drained in the same tick (completed_at set)
    // or is still pending. Either is correct — both states are idempotent on
    // retry.
    let (queue_exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM clickhouse_purge_queue WHERE org_id = $1)")
            .bind(org.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(queue_exists, "purge queue should track the cascaded org");

    // Cleanup: drop queue row + user.
    sqlx::query("DELETE FROM clickhouse_purge_queue WHERE org_id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn restore_cancels_purge() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let org = create_org_with_owner(&pool, user, &unique_slug("cncl"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    soft_delete_org(&pool, org.id, user).await.unwrap();
    // Re-activate before grace window expires.
    sqlx::query("UPDATE organizations SET deleted_at = NULL WHERE id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    // Even if we somehow had a past-grace timestamp, deleted_at IS NULL filter
    // wins: the purge query never sees this row.
    let stats = purge_tick(&pool, &ch, 30).await.unwrap();
    assert_eq!(stats.cascaded, 0);

    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn drain_is_idempotent_on_repeat() {
    // Calling drain twice on the same queue row should mark it complete once
    // and leave the second call as a no-op (no rows pending).
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let org = create_org_with_owner(&pool, user, &unique_slug("drn"), "n", 3)
        .await
        .unwrap()
        .unwrap();
    soft_delete_org(&pool, org.id, user).await.unwrap();
    backdate_delete(&pool, org.id, 40).await;

    let _ = purge_tick(&pool, &ch, 30).await.unwrap();
    // Force the queue row back to pending to simulate a worker that died
    // before marking complete, then re-drain.
    sqlx::query("UPDATE clickhouse_purge_queue SET completed_at = NULL WHERE org_id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    let drained = drain_clickhouse_purge_queue(&pool, &ch).await.unwrap();
    assert!(drained >= 1, "second drain should mark the row complete");
    let drained_again = drain_clickhouse_purge_queue(&pool, &ch).await.unwrap();
    assert_eq!(drained_again, 0, "third drain has nothing pending");

    sqlx::query("DELETE FROM clickhouse_purge_queue WHERE org_id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn enqueue_is_dedup_on_conflict() {
    // ON CONFLICT (org_id) DO NOTHING — a second enqueue for the same org
    // mustn't create a duplicate row.
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let org = create_org_with_owner(&pool, user, &unique_slug("dup"), "n", 3)
        .await
        .unwrap()
        .unwrap();

    sqlx::query(
        "INSERT INTO clickhouse_purge_queue (org_id) VALUES ($1) ON CONFLICT (org_id) DO NOTHING",
    )
    .bind(org.id.0)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO clickhouse_purge_queue (org_id) VALUES ($1) ON CONFLICT (org_id) DO NOTHING",
    )
    .bind(org.id.0)
    .execute(&pool)
    .await
    .unwrap();

    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM clickhouse_purge_queue WHERE org_id = $1")
            .bind(org.id.0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);

    sqlx::query("DELETE FROM clickhouse_purge_queue WHERE org_id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}
