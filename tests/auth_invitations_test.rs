//! Live-PG tests for Phase 5: invitation issuance/lookup/accept/decline/expiry.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test auth_invitations_test -- --ignored

mod common;

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection};
use status_monitor::auth::invitations;
use status_monitor::domain::{OrgId, Role, UserId, generate_personal_slug};
use status_monitor::storage::orgs::{
    self as orgs_store, AddMemberOutcome, create_personal_org_with_owner_in_tx,
};
use url::Url;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    let raw = std::env::var("DATABASE_URL").ok()?;
    let mut url = Url::parse(&raw).unwrap();
    let test_db = format!("auth_inv_{}", Uuid::now_v7().simple());
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

async fn seed_user(pool: &sqlx::PgPool, email: &str) -> UserId {
    let (id,): (Uuid,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap();
    UserId(id)
}

async fn seed_org(pool: &sqlx::PgPool, owner: UserId) -> OrgId {
    let mut tx = pool.begin().await.unwrap();
    let org = loop {
        let slug = generate_personal_slug();
        if let Some(o) = create_personal_org_with_owner_in_tx(&mut tx, owner, &slug, "T")
            .await
            .unwrap()
        {
            break o;
        }
    };
    tx.commit().await.unwrap();
    org
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn create_lookup_accept_flow() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "owner@example.test").await;
    let org = seed_org(&pool, owner).await;

    let created = invitations::create(&pool, org, owner, "Alice@Example.test", Role::Member, 168)
        .await
        .expect("create");
    assert!(!created.token.is_empty());

    // Find pending must locate exactly one row by raw token.
    let pending = invitations::find_pending_by_token(&pool, &created.token)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(pending.id, created.row.id);
    assert_eq!(pending.role, Role::Member);

    // Recipient signs up (CITEXT — lower-case in users table).
    let alice = seed_user(&pool, "alice@example.test").await;
    let added = orgs_store::add_member(&pool, org, alice, alice, Role::Member)
        .await
        .unwrap();
    assert_eq!(added, AddMemberOutcome::Added);
    let accepted = invitations::mark_accepted(&pool, created.row.id)
        .await
        .unwrap();
    assert!(accepted);

    // Second accept attempt fails — row is no longer pending.
    let again = invitations::mark_accepted(&pool, created.row.id)
        .await
        .unwrap();
    assert!(!again);
    let lookup = invitations::find_pending_by_token(&pool, &created.token)
        .await
        .unwrap();
    assert!(
        lookup.is_none(),
        "accepted invitation must not match lookup"
    );

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn decline_flow() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "o@example.test").await;
    let org = seed_org(&pool, owner).await;
    let inv = invitations::create(&pool, org, owner, "x@example.test", Role::Member, 168)
        .await
        .unwrap();

    let declined = invitations::mark_declined(&pool, inv.row.id).await.unwrap();
    assert!(declined);

    let lookup = invitations::find_pending_by_token(&pool, &inv.token)
        .await
        .unwrap();
    assert!(lookup.is_none());

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn expired_invitation_is_not_pending() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "o2@example.test").await;
    let org = seed_org(&pool, owner).await;
    let inv = invitations::create(&pool, org, owner, "y@example.test", Role::Member, 168)
        .await
        .unwrap();
    sqlx::query("UPDATE invitations SET expires_at = now() - INTERVAL '1 hour' WHERE id = $1")
        .bind(inv.row.id)
        .execute(&pool)
        .await
        .unwrap();

    let lookup = invitations::find_pending_by_token(&pool, &inv.token)
        .await
        .unwrap();
    assert!(lookup.is_none());
    assert!(
        !invitations::mark_accepted(&pool, inv.row.id).await.unwrap(),
        "expired must not accept"
    );

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn already_invited_blocks_second_pending() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "o3@example.test").await;
    let org = seed_org(&pool, owner).await;
    invitations::create(&pool, org, owner, "z@example.test", Role::Member, 168)
        .await
        .unwrap();
    assert!(
        invitations::exists_pending_for_email(&pool, org, "Z@Example.test")
            .await
            .unwrap(),
        "CITEXT match must catch the second invite",
    );

    pool.close().await;
    drop_pg(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn purge_old_drops_settled_and_expired_rows() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "o4@example.test").await;
    let org = seed_org(&pool, owner).await;
    let old_accepted =
        invitations::create(&pool, org, owner, "old1@example.test", Role::Member, 168)
            .await
            .unwrap();
    let old_expired =
        invitations::create(&pool, org, owner, "old2@example.test", Role::Member, 168)
            .await
            .unwrap();
    let fresh = invitations::create(&pool, org, owner, "fresh@example.test", Role::Member, 168)
        .await
        .unwrap();

    sqlx::query("UPDATE invitations SET accepted_at = now() - INTERVAL '90 days' WHERE id = $1")
        .bind(old_accepted.row.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE invitations SET expires_at = now() - INTERVAL '90 days' WHERE id = $1")
        .bind(old_expired.row.id)
        .execute(&pool)
        .await
        .unwrap();

    let removed = invitations::purge_old(&pool, 30).await.unwrap();
    assert!(
        removed >= 2,
        "expected the two old rows to be removed (got {removed})"
    );

    let (n_fresh,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM invitations WHERE id = $1")
        .bind(fresh.row.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n_fresh, 1, "fresh row must survive");

    pool.close().await;
    drop_pg(&name).await;
}
