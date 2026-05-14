//! Live-PG tests for Phase 4: API token issuance, prefix lookup with
//! deliberate collisions, argon2 verification, revoke, max_per_user.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test auth_api_tokens_test -- --ignored

mod common;

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection};
use status_monitor::auth::api_tokens;
use status_monitor::domain::UserId;
use url::Url;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    let raw = std::env::var("DATABASE_URL").ok()?;
    let mut url = Url::parse(&raw).expect("DATABASE_URL must be a valid URL");
    let test_db = format!("auth_tokens_{}", Uuid::now_v7().simple());
    url.set_path("/postgres");
    let admin = url.clone();
    let mut conn = PgConnection::connect(admin.as_str()).await.unwrap();
    conn.execute(format!("CREATE DATABASE {test_db}").as_str())
        .await
        .unwrap();
    let mut new_url = admin.clone();
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

async fn seed_user(pool: &sqlx::PgPool) -> UserId {
    let (id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(format!("u-{}@example.test", Uuid::now_v7()))
        .fetch_one(pool)
        .await
        .unwrap();
    UserId(id)
}

const PREFIX_LEN: usize = 16;

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn create_then_lookup_and_revoke_roundtrip() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool).await;
    let created = api_tokens::create(&pool, user, "CI", PREFIX_LEN)
        .await
        .expect("create");
    assert!(created.token.starts_with(api_tokens::TOKEN_PREFIX));
    assert_eq!(created.token.len(), 8 + 43);
    assert_eq!(created.prefix.len(), PREFIX_LEN);

    let outcome = api_tokens::lookup_by_raw(&pool, &created.token, PREFIX_LEN)
        .await
        .expect("lookup");
    match outcome {
        api_tokens::LookupOutcome::Active(row) => {
            assert_eq!(row.id, created.id);
            assert_eq!(row.user_id.0, user.0);
        }
        api_tokens::LookupOutcome::Invalid => panic!("lookup should match"),
    }

    // Wrong-token returns Invalid (not the matching row), even if same prefix.
    let bogus = format!(
        "{}wrongwrongwrongwrongwrongwrongwrongwrong",
        &created.prefix
    );
    let bogus_outcome = api_tokens::lookup_by_raw(&pool, &bogus, PREFIX_LEN)
        .await
        .expect("lookup");
    assert!(matches!(bogus_outcome, api_tokens::LookupOutcome::Invalid));

    let removed = api_tokens::delete_for_user(&pool, user, created.id)
        .await
        .expect("delete");
    assert!(removed);

    let after = api_tokens::lookup_by_raw(&pool, &created.token, PREFIX_LEN)
        .await
        .expect("lookup");
    assert!(matches!(after, api_tokens::LookupOutcome::Invalid));

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn forced_prefix_collision_still_finds_correct_token() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool).await;
    // Issue real token, then manually plant a sibling row with the same
    // prefix but a hash for "wrong" so the lookup must disambiguate by
    // argon2-verify, not by prefix alone.
    let created = api_tokens::create(&pool, user, "real", PREFIX_LEN)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO api_tokens (user_id, name, token_hash, token_prefix) \
         VALUES ($1, 'collider', '$argon2id$v=19$m=19456,t=2,p=1$YzhrXTk5SUVHWUMzbHkwTw$bogus', $2)",
    )
    .bind(user.0)
    .bind(&created.prefix)
    .execute(&pool)
    .await
    .unwrap();

    let outcome = api_tokens::lookup_by_raw(&pool, &created.token, PREFIX_LEN)
        .await
        .expect("lookup");
    match outcome {
        api_tokens::LookupOutcome::Active(row) => assert_eq!(row.id, created.id),
        api_tokens::LookupOutcome::Invalid => panic!("must find the real token"),
    }

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn count_for_user_grows_then_revoke_drops() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool).await;
    assert_eq!(api_tokens::count_for_user(&pool, user).await.unwrap(), 0);

    let t1 = api_tokens::create(&pool, user, "t1", PREFIX_LEN)
        .await
        .unwrap();
    let _t2 = api_tokens::create(&pool, user, "t2", PREFIX_LEN)
        .await
        .unwrap();
    assert_eq!(api_tokens::count_for_user(&pool, user).await.unwrap(), 2);

    let removed = api_tokens::delete_for_user(&pool, user, t1.id)
        .await
        .unwrap();
    assert!(removed);
    assert_eq!(api_tokens::count_for_user(&pool, user).await.unwrap(), 1);

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn rename_only_succeeds_for_owner() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool).await;
    let other = seed_user(&pool).await;
    let tok = api_tokens::create(&pool, owner, "ci", PREFIX_LEN)
        .await
        .unwrap();
    let foreign = api_tokens::rename_for_user(&pool, other, tok.id, "stolen")
        .await
        .unwrap();
    assert!(!foreign, "other user must not rename");
    let mine = api_tokens::rename_for_user(&pool, owner, tok.id, "renamed")
        .await
        .unwrap();
    assert!(mine);

    let listing = api_tokens::list_for_user(&pool, owner).await.unwrap();
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].name, "renamed");

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn expired_token_lookup_returns_invalid() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = seed_user(&pool).await;
    let tok = api_tokens::create(&pool, user, "expiring", PREFIX_LEN)
        .await
        .unwrap();
    sqlx::query("UPDATE api_tokens SET expires_at = now() - INTERVAL '1 minute' WHERE id = $1")
        .bind(tok.id)
        .execute(&pool)
        .await
        .unwrap();
    let outcome = api_tokens::lookup_by_raw(&pool, &tok.token, PREFIX_LEN)
        .await
        .unwrap();
    assert!(matches!(outcome, api_tokens::LookupOutcome::Invalid));

    pool.close().await;
    drop_pg(&name).await;
}
