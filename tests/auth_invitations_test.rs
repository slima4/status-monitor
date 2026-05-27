//! Live-PG tests for Phase 5: invitation issuance/lookup/accept/decline/expiry.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test auth_invitations_test -- --ignored

mod common;

use status_monitor::auth::invitations;
use status_monitor::domain::{OrgId, Role, UserId, generate_signup_slug};
use status_monitor::storage::orgs::{
    self as orgs_store, AddMemberOutcome, create_signup_org_with_owner_in_tx,
};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    common::fresh_test_db("auth_inv").await
}

async fn drop_pg(test_db: &str) {
    common::drop_test_db(test_db).await;
}

async fn open_pool(db_url: &str) -> sqlx::PgPool {
    common::open_test_pool(db_url).await
}

async fn seed_user(pool: &sqlx::PgPool, email: &str) -> UserId {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, terms_version, privacy_version) \
         VALUES ($1, 'v1', 'v1') RETURNING id",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .unwrap();
    UserId(id)
}

async fn seed_org(pool: &sqlx::PgPool, owner: UserId) -> OrgId {
    let mut tx = pool.begin().await.unwrap();
    let org = loop {
        let slug = generate_signup_slug();
        if let Some(o) = create_signup_org_with_owner_in_tx(&mut tx, owner, &slug, "T")
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

    let created = invitations::create(
        &pool,
        org,
        owner,
        "Alice@Example.test",
        Role::Member,
        168,
        u32::MAX,
    )
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
    let added = orgs_store::add_member(&pool, org, alice, alice, Role::Member, u32::MAX)
        .await
        .unwrap();
    assert_eq!(added, AddMemberOutcome::Added);
    let accepted = invitations::mark_accepted(&pool, org, created.row.id)
        .await
        .unwrap();
    assert!(accepted);

    // Second accept attempt fails — row is no longer pending.
    let again = invitations::mark_accepted(&pool, org, created.row.id)
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
    let inv = invitations::create(
        &pool,
        org,
        owner,
        "x@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
    .await
    .unwrap();

    let declined = invitations::mark_declined(&pool, org, inv.row.id)
        .await
        .unwrap();
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
    let inv = invitations::create(
        &pool,
        org,
        owner,
        "y@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
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
        !invitations::mark_accepted(&pool, org, inv.row.id)
            .await
            .unwrap(),
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
    invitations::create(
        &pool,
        org,
        owner,
        "z@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
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
    let old_accepted = invitations::create(
        &pool,
        org,
        owner,
        "old1@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
    .await
    .unwrap();
    let old_expired = invitations::create(
        &pool,
        org,
        owner,
        "old2@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
    .await
    .unwrap();
    let fresh = invitations::create(
        &pool,
        org,
        owner,
        "fresh@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
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

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn mark_accepted_and_declined_require_matching_org() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let owner = seed_user(&pool, "tenant_a@example.test").await;
    let org_a = seed_org(&pool, owner).await;
    let attacker_owner = seed_user(&pool, "tenant_b@example.test").await;
    let org_b = seed_org(&pool, attacker_owner).await;

    let inv = invitations::create(
        &pool,
        org_a,
        owner,
        "victim@example.test",
        Role::Member,
        168,
        u32::MAX,
    )
    .await
    .unwrap();

    assert!(
        !invitations::mark_accepted(&pool, org_b, inv.row.id)
            .await
            .unwrap(),
        "wrong-org accept must be a no-op",
    );
    assert!(
        !invitations::mark_declined(&pool, org_b, inv.row.id)
            .await
            .unwrap(),
        "wrong-org decline must be a no-op",
    );
    assert!(
        invitations::find_pending_by_token(&pool, &inv.token)
            .await
            .unwrap()
            .is_some(),
        "invitation must still be pending after cross-tenant attempts",
    );

    assert!(
        invitations::mark_accepted(&pool, org_a, inv.row.id)
            .await
            .unwrap(),
        "matching-org accept must succeed",
    );

    pool.close().await;
    drop_pg(&name).await;
}
