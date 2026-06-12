//! Live-PG tests for Phase 8 magic-link primitives.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test auth_magic_link_test -- --ignored

mod common;

use uptimepage::auth::magic_link;
use uptimepage::storage::orgs as orgs_store;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    common::fresh_test_db("auth_ml").await
}

async fn drop_pg(test_db: &str) {
    common::drop_test_db(test_db).await;
}

async fn open_pool(db_url: &str) -> sqlx::PgPool {
    common::open_test_pool(db_url).await
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn create_then_consume_roundtrip() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let created = magic_link::create(&pool, "alice@example.test", None, 15, None, None)
        .await
        .expect("create");

    let consumed = magic_link::consume(&pool, &created.token)
        .await
        .expect("consume")
        .expect("row");
    assert_eq!(consumed.email, "alice@example.test");
    assert_eq!(consumed.id, created.row.id);

    // Second consume of the same token is rejected — single-use.
    assert!(
        magic_link::consume(&pool, &created.token)
            .await
            .expect("consume2")
            .is_none(),
        "token must be single-use"
    );

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn consume_with_unknown_token_returns_none() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    // Seed a real row so the prefix-narrowed SELECT has something to compare
    // against — otherwise an empty table never enters the argon2-verify loop
    // and the test couldn't catch a verify-mismatch regression.
    let _real = magic_link::create(&pool, "real@example.test", None, 15, None, None)
        .await
        .expect("seed");

    let outcome = magic_link::consume(&pool, "this-token-was-never-issued")
        .await
        .expect("consume");
    assert!(outcome.is_none());

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn consume_skips_expired_rows() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let created = magic_link::create(&pool, "bob@example.test", None, 15, None, None)
        .await
        .expect("create");

    // Force-expire the row.
    sqlx::query(
        "UPDATE magic_link_tokens SET expires_at = now() - INTERVAL '1 minute' WHERE id = $1",
    )
    .bind(created.row.id)
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        magic_link::consume(&pool, &created.token)
            .await
            .expect("consume")
            .is_none(),
        "expired token must not redeem"
    );

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn purge_old_removes_expired_and_old_used() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    // Row 1: expired, unused.
    let r1 = magic_link::create(&pool, "x1@example.test", None, 15, None, None)
        .await
        .expect("create");
    sqlx::query(
        "UPDATE magic_link_tokens SET expires_at = now() - INTERVAL '1 minute' WHERE id = $1",
    )
    .bind(r1.row.id)
    .execute(&pool)
    .await
    .unwrap();

    // Row 2: used 8 days ago.
    let r2 = magic_link::create(&pool, "x2@example.test", None, 15, None, None)
        .await
        .expect("create");
    sqlx::query("UPDATE magic_link_tokens SET used_at = now() - INTERVAL '8 days' WHERE id = $1")
        .bind(r2.row.id)
        .execute(&pool)
        .await
        .unwrap();

    // Row 3: fresh, unused — must survive.
    let r3 = magic_link::create(&pool, "x3@example.test", None, 15, None, None)
        .await
        .expect("create");

    let removed = magic_link::purge_old(&pool).await.unwrap();
    assert_eq!(removed, 2);

    let row_count: i64 = sqlx::query_scalar("SELECT count(*) FROM magic_link_tokens")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count, 1);

    let survivor: Uuid = sqlx::query_scalar("SELECT id FROM magic_link_tokens")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(survivor, r3.row.id);

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn earliest_in_window_picks_first_row_and_excludes_others() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    // Two requests for the same email back-to-back.
    let first = magic_link::create(&pool, "throttle@example.test", None, 15, None, None)
        .await
        .expect("first create");
    let second = magic_link::create(&pool, "throttle@example.test", None, 15, None, None)
        .await
        .expect("second create");
    assert_ne!(first.row.id, second.row.id);

    // Inside the 60s window: first row wins for both spawn checks.
    let winner = magic_link::earliest_in_window(&pool, "throttle@example.test", 60)
        .await
        .expect("earliest");
    assert_eq!(winner, Some(first.row.id));

    // A different email is not throttled.
    let other = magic_link::create(&pool, "other@example.test", None, 15, None, None)
        .await
        .expect("other create");
    let other_winner = magic_link::earliest_in_window(&pool, "other@example.test", 60)
        .await
        .expect("other earliest");
    assert_eq!(other_winner, Some(other.row.id));

    // window_seconds = 0 disables (operator escape hatch).
    let disabled = magic_link::earliest_in_window(&pool, "throttle@example.test", 0)
        .await
        .expect("disabled");
    assert!(disabled.is_none());

    // Force the first row outside the window — the second now wins.
    sqlx::query(
        "UPDATE magic_link_tokens SET created_at = now() - INTERVAL '2 minutes' WHERE id = $1",
    )
    .bind(first.row.id)
    .execute(&pool)
    .await
    .unwrap();
    let post_expiry = magic_link::earliest_in_window(&pool, "throttle@example.test", 60)
        .await
        .expect("post expiry");
    assert_eq!(post_expiry, Some(second.row.id));

    // Mark second as used → no winner remains in the window.
    sqlx::query("UPDATE magic_link_tokens SET used_at = now() WHERE id = $1")
        .bind(second.row.id)
        .execute(&pool)
        .await
        .unwrap();
    let after_use = magic_link::earliest_in_window(&pool, "throttle@example.test", 60)
        .await
        .expect("after use");
    assert!(after_use.is_none());

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn earliest_in_window_under_concurrent_inserts_picks_one_winner() {
    // Locks the race contract that the spawn-based throttle relies on:
    // two parallel INSERTs for the same email must resolve to exactly one
    // winner under `earliest_in_window`. If the ORDER BY ever loses its
    // total-order property, this test fails — the throttle's "one email
    // per window" guarantee would break with it.
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (a, b) = tokio::join!(
        magic_link::create(&pool, "race@example.test", None, 15, None, None),
        magic_link::create(&pool, "race@example.test", None, 15, None, None),
    );
    let a = a.expect("a create");
    let b = b.expect("b create");
    assert_ne!(a.row.id, b.row.id);

    let winner = magic_link::earliest_in_window(&pool, "race@example.test", 60)
        .await
        .expect("earliest")
        .expect("a winner exists");
    assert!(
        winner == a.row.id || winner == b.row.id,
        "winner must be one of the two inserted rows"
    );

    // Every spawn would converge on the same winner — deterministic.
    let again = magic_link::earliest_in_window(&pool, "race@example.test", 60)
        .await
        .expect("earliest again")
        .expect("still a winner");
    assert_eq!(winner, again, "winner must be stable across reads");

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn tombstone_lookup_and_undelete_restore_for_magic_link_verify() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let (user_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, terms_version, privacy_version) \
         VALUES ('Frank@Example.test', 'v1', 'v1') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("seed user");
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("soft-delete");

    // Invitation semantics stay active-only: tombstoned user is invisible.
    assert!(
        orgs_store::find_user_by_email(&pool, "frank@example.test")
            .await
            .expect("active lookup")
            .is_none()
    );

    // The verify path's lookup sees the tombstone and restores it.
    let (found, deleted_at) =
        orgs_store::find_user_by_email_including_deleted(&pool, "frank@example.test")
            .await
            .expect("tombstone lookup")
            .expect("row");
    assert_eq!(found.0, user_id);
    assert!(deleted_at.is_some());

    let mut tx = pool.begin().await.unwrap();
    uptimepage::auth::account::undelete_in_tx(&mut tx, found)
        .await
        .expect("undelete");
    tx.commit().await.unwrap();

    assert_eq!(
        orgs_store::find_user_by_email(&pool, "frank@example.test")
            .await
            .expect("post-restore lookup")
            .map(|u| u.0),
        Some(user_id)
    );

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn tombstone_lookup_prefers_active_row_over_tombstone() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    // Same email twice: a tombstoned row (older) and an active row — the
    // partial unique index permits this pair. The lookup must never pick
    // the tombstone when an active account exists.
    sqlx::query(
        "INSERT INTO users (email, terms_version, privacy_version, deleted_at, created_at) \
         VALUES ('Gus@Example.test', 'v1', 'v1', now(), now() - INTERVAL '1 day')",
    )
    .execute(&pool)
    .await
    .expect("seed tombstone");
    let (active_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, terms_version, privacy_version) \
         VALUES ('gus@example.test', 'v1', 'v1') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("seed active");

    let (found, deleted_at) =
        orgs_store::find_user_by_email_including_deleted(&pool, "gus@example.test")
            .await
            .expect("lookup")
            .expect("row");
    assert_eq!(found.0, active_id, "active row must win over tombstone");
    assert!(deleted_at.is_none());

    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn redirect_and_invitation_round_trip_through_create_and_consume() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let created = magic_link::create(
        &pool,
        "hop@example.test",
        None,
        15,
        Some("/targets/abc"),
        None,
    )
    .await
    .expect("create");
    let row = magic_link::consume(&pool, &created.token)
        .await
        .expect("consume")
        .expect("row");
    assert_eq!(row.redirect_after.as_deref(), Some("/targets/abc"));
    assert_eq!(row.invitation_id, None);

    drop_pg(&name).await;
}
