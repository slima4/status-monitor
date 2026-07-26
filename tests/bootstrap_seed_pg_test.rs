//! Live-PG tests for unattended first-run owner seeding.
//!
//! Run via:
//!     docker compose -f compose.dev.yml up -d postgres
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo nextest run --test bootstrap_seed_pg_test --run-ignored all

mod common;

use sqlx::PgPool;
use uptimepage::bootstrap::seed_first_owner;
use uptimepage::config::AppConfig;
use uptimepage::storage::{orgs as orgs_store, users};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pool() -> Option<(PgPool, String)> {
    let (db, name) = common::fresh_test_db("bootstrap_seed").await?;
    let pool = common::open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    Some((pool, name))
}

fn cfg_with(email: &str) -> AppConfig {
    let mut cfg = AppConfig::load().expect("config");
    cfg.bootstrap.email = email.to_string();
    cfg.bootstrap.org_name = "Home Lab".to_string();
    cfg
}

async fn user_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn link_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM magic_link_tokens")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn seeds_owner_org_and_a_usable_link() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };

    seed_first_owner(&pool, &cfg_with("owner@example.test"))
        .await
        .expect("seed");

    assert_eq!(user_count(&pool).await, 1);
    assert_eq!(link_count(&pool).await, 1);

    let (user, created) = users::create_invited_user(&pool, "owner@example.test")
        .await
        .expect("lookup owner");
    assert!(!created, "seeding must have created the owner already");

    let org = orgs_store::oldest_membership_for_user(&pool, user)
        .await
        .expect("membership")
        .expect("owner must own an org");
    let row = orgs_store::get_org(&pool, org)
        .await
        .expect("get_org")
        .expect("org row");
    assert_eq!(row.name, "Home Lab");

    // The link lands in the log stream and may sit unread for hours, so it gets
    // a longer life than an interactive login.
    let hours: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM expires_at - now()) / 3600)::float8 FROM magic_link_tokens",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((23.0..=24.0).contains(&hours), "expiry was {hours}h");

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn seeding_is_one_shot_across_restarts() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };
    let cfg = cfg_with("owner@example.test");

    seed_first_owner(&pool, &cfg).await.expect("first boot");
    seed_first_owner(&pool, &cfg).await.expect("second boot");
    seed_first_owner(&pool, &cfg).await.expect("third boot");

    assert_eq!(user_count(&pool).await, 1, "owner seeded once");
    assert_eq!(
        link_count(&pool).await,
        1,
        "every restart must not mint a new sign-in link"
    );

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn soft_deleted_owner_does_not_reopen_seeding() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };
    let cfg = cfg_with("owner@example.test");
    seed_first_owner(&pool, &cfg).await.expect("first boot");

    sqlx::query("UPDATE users SET deleted_at = now()")
        .execute(&pool)
        .await
        .unwrap();

    seed_first_owner(&pool, &cfg)
        .await
        .expect("boot after wipe");
    assert_eq!(
        link_count(&pool).await,
        1,
        "a deleted owner must not re-arm seeding"
    );

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn existing_unrelated_user_blocks_seeding() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };
    users::create_invited_user(&pool, "someone@example.test")
        .await
        .expect("pre-existing user");

    seed_first_owner(&pool, &cfg_with("owner@example.test"))
        .await
        .expect("seed");

    assert_eq!(user_count(&pool).await, 1);
    assert_eq!(link_count(&pool).await, 0);

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn empty_email_disables_seeding() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };

    seed_first_owner(&pool, &cfg_with("  "))
        .await
        .expect("seed");

    assert_eq!(user_count(&pool).await, 0);
    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn malformed_email_fails_boot() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };

    let err = seed_first_owner(&pool, &cfg_with("not-an-email"))
        .await
        .expect_err("must not seed an unreachable account");
    assert!(err.to_string().contains("not an email address"), "{err}");

    assert_eq!(user_count(&pool).await, 0);
    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn bad_config_is_inert_once_the_instance_is_claimed() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };
    seed_first_owner(&pool, &cfg_with("owner@example.test"))
        .await
        .expect("first boot");

    // A stale or corrupted value must not take down an already-claimed instance.
    seed_first_owner(&pool, &cfg_with("not-an-email"))
        .await
        .expect("malformed email after claiming");

    let mut no_magic = cfg_with("owner@example.test");
    no_magic.auth.enabled_methods.retain(|m| m != "magic_link");
    seed_first_owner(&pool, &no_magic)
        .await
        .expect("magic link off after claiming");

    assert_eq!(user_count(&pool).await, 1);
    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn magic_link_disabled_fails_boot() {
    let Some((pool, name)) = fresh_pool().await else {
        return;
    };
    let mut cfg = cfg_with("owner@example.test");
    cfg.auth.enabled_methods.retain(|m| m != "magic_link");

    let err = seed_first_owner(&pool, &cfg)
        .await
        .expect_err("no way to hand back a sign-in link");
    assert!(err.to_string().contains("magic_link"), "{err}");

    assert_eq!(user_count(&pool).await, 0, "must fail before writing");
    common::drop_test_db(&name).await;
}
