//! Live-PG tests for Phase 8 magic-link primitives.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test auth_magic_link_test -- --ignored

mod common;

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection};
use status_monitor::auth::magic_link;
use url::Url;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    let raw = std::env::var("DATABASE_URL").ok()?;
    let mut url = Url::parse(&raw).unwrap();
    let test_db = format!("auth_ml_{}", Uuid::now_v7().simple());
    url.set_path("/postgres");
    let mut conn = PgConnection::connect(url.as_str()).await.unwrap();
    conn.execute(format!("CREATE DATABASE {test_db}").as_str())
        .await
        .unwrap();
    let mut new_url = url.clone();
    new_url.set_path(&format!("/{test_db}"));
    Some((new_url.to_string(), test_db))
}

async fn drop_pg(test_db: &str) {
    let Ok(raw) = std::env::var("DATABASE_URL") else {
        return;
    };
    let mut url = Url::parse(&raw).unwrap();
    url.set_path("/postgres");
    if let Ok(mut conn) = PgConnection::connect(url.as_str()).await {
        let _ = conn
            .execute(
                format!(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = '{test_db}' AND pid <> pg_backend_pid()"
                )
                .as_str(),
            )
            .await;
        let _ = conn
            .execute(format!("DROP DATABASE IF EXISTS {test_db}").as_str())
            .await;
    }
}

async fn open_pool(db_url: &str) -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
        .connect(db_url)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn create_then_consume_roundtrip() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let created = magic_link::create(&pool, "alice@example.test", None, 15)
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
    let _real = magic_link::create(&pool, "real@example.test", None, 15)
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

    let created = magic_link::create(&pool, "bob@example.test", None, 15)
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
    let r1 = magic_link::create(&pool, "x1@example.test", None, 15)
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
    let r2 = magic_link::create(&pool, "x2@example.test", None, 15)
        .await
        .expect("create");
    sqlx::query("UPDATE magic_link_tokens SET used_at = now() - INTERVAL '8 days' WHERE id = $1")
        .bind(r2.row.id)
        .execute(&pool)
        .await
        .unwrap();

    // Row 3: fresh, unused — must survive.
    let r3 = magic_link::create(&pool, "x3@example.test", None, 15)
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
