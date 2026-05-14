//! Live-Postgres tests for the org-management storage layer. Skipped at the
//! `cargo test` default; runs under `--include-ignored` once `DATABASE_URL` is
//! set. The tests share a database with the rest of the live-PG suite, so
//! every test seeds its own users + orgs with `Uuid::now_v7()` slugs and
//! cleans up via `ON DELETE CASCADE` from a final `DELETE FROM users`.

mod common;

use status_monitor::domain::{OrgId, Role, UserId};
use status_monitor::storage::orgs as orgs_store;
use status_monitor::storage::{
    RemoveOutcome, RestoreOutcome, create_org_with_owner, ensure_default_org, is_active_member,
    is_owner, list_deleted_orgs_deleted_by, list_members, list_orgs_for_user, owner_org_count,
    personal_org_for_user, remove_member, restore_org, slug_is_available, soft_delete_org,
    update_org_name,
};
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
    // Take the tail of v4 (pure-random) hex so two slugs minted in the same
    // millisecond don't collide — v7's leading bytes are the timestamp.
    let id = Uuid::new_v4().simple().to_string();
    let suffix = &id[id.len() - 6..];
    format!("{prefix}-{suffix}")
}

#[tokio::test]
#[ignore]
async fn create_org_and_slug_check_round_trip() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let slug = unique_slug("acme");

    assert!(slug_is_available(&pool, &slug).await.unwrap());
    let created = create_org_with_owner(&pool, user, &slug, "Acme", 3)
        .await
        .unwrap()
        .expect("created");
    assert_eq!(created.slug, slug.to_ascii_lowercase());
    assert!(!slug_is_available(&pool, &slug).await.unwrap());

    // Second attempt at the same slug collides → None.
    let collision = create_org_with_owner(&pool, user, &slug, "Acme2", 3)
        .await
        .unwrap();
    assert!(collision.is_none(), "expected slug-collision None");

    // Caller is now an active member + owner.
    assert!(is_active_member(&pool, user, created.id).await.unwrap());
    assert!(is_owner(&pool, user, created.id).await.unwrap());

    // Cleanup
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn owner_org_limit_is_atomic() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    // Limit 2 — first two creates succeed, third fails with OWNER_ORG_LIMIT.
    let s1 = unique_slug("lim");
    let s2 = unique_slug("lim");
    let s3 = unique_slug("lim");
    create_org_with_owner(&pool, user, &s1, "one", 2)
        .await
        .unwrap()
        .expect("first");
    create_org_with_owner(&pool, user, &s2, "two", 2)
        .await
        .unwrap()
        .expect("second");
    let err = create_org_with_owner(&pool, user, &s3, "three", 2)
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("OWNER_ORG_LIMIT"), "got {msg}");

    assert_eq!(owner_org_count(&pool, user).await.unwrap(), 2);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn list_orgs_excludes_soft_deleted_and_update_name_works() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let slug = unique_slug("list");
    let org = create_org_with_owner(&pool, user, &slug, "Old name", 3)
        .await
        .unwrap()
        .unwrap();

    let listed = list_orgs_for_user(&pool, user).await.unwrap();
    assert!(
        listed
            .iter()
            .any(|o| o.org.id == org.id && o.role == Role::Owner)
    );

    let renamed = update_org_name(&pool, org.id, user, "New name")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed.name, "New name");

    assert!(soft_delete_org(&pool, org.id, user).await.unwrap());
    // Now hidden from active list, surfaced in deleted list.
    let listed = list_orgs_for_user(&pool, user).await.unwrap();
    assert!(listed.iter().all(|o| o.org.id != org.id));
    let deleted = list_deleted_orgs_deleted_by(&pool, user).await.unwrap();
    assert!(deleted.iter().any(|o| o.id == org.id));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn restore_org_inside_window_succeeds() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let slug = unique_slug("rest");
    let org = create_org_with_owner(&pool, user, &slug, "n", 3)
        .await
        .unwrap()
        .unwrap();
    soft_delete_org(&pool, org.id, user).await.unwrap();

    let outcome = restore_org(&pool, org.id, user, 30).await.unwrap();
    assert!(matches!(outcome, RestoreOutcome::Restored(ref o) if o.id == org.id));
    assert!(is_active_member(&pool, user, org.id).await.unwrap());

    // Restoring an already-active org reports NotDeleted, not Restored.
    let again = restore_org(&pool, org.id, user, 30).await.unwrap();
    assert!(matches!(again, RestoreOutcome::NotDeleted));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn restore_org_outside_window_refuses() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let slug = unique_slug("expr");
    let org = create_org_with_owner(&pool, user, &slug, "n", 3)
        .await
        .unwrap()
        .unwrap();
    soft_delete_org(&pool, org.id, user).await.unwrap();
    // Backdate the deletion past the grace window.
    sqlx::query("UPDATE organizations SET deleted_at = now() - INTERVAL '40 days' WHERE id = $1")
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();

    let outcome = restore_org(&pool, org.id, user, 30).await.unwrap();
    assert!(matches!(outcome, RestoreOutcome::WindowExpired));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn remove_member_refuses_last_owner() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool).await;
    let slug = unique_slug("solo");
    let org = create_org_with_owner(&pool, user, &slug, "Solo", 3)
        .await
        .unwrap()
        .unwrap();

    let outcome = remove_member(&pool, org.id, user, user).await.unwrap();
    assert_eq!(outcome, RemoveOutcome::LastOwner);
    assert!(is_owner(&pool, user, org.id).await.unwrap(), "still owner");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn remove_member_succeeds_when_other_owners_exist() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let a = make_user(&pool).await;
    let b = make_user(&pool).await;
    let slug = unique_slug("pair");
    let org = create_org_with_owner(&pool, a, &slug, "Pair", 3)
        .await
        .unwrap()
        .unwrap();
    // Manually add `b` as a second owner (no public API yet).
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'owner')")
        .bind(b.0)
        .bind(org.id.0)
        .execute(&pool)
        .await
        .unwrap();

    let outcome = remove_member(&pool, org.id, a, b).await.unwrap();
    assert_eq!(outcome, RemoveOutcome::Removed);
    let members = list_members(&pool, org.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].membership.user_id, a);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(&[a.0, b.0][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn ensure_default_org_idempotent_and_personal_org_lookup_returns_none() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let first: OrgId = ensure_default_org(&pool, "default").await.unwrap();
    let second: OrgId = ensure_default_org(&pool, "default").await.unwrap();
    assert_eq!(first, second);

    // A brand-new user has no personal org until signup auto-creates one.
    let user = make_user(&pool).await;
    assert!(personal_org_for_user(&pool, user).await.unwrap().is_none());

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

// Silence unused-import warnings when none of the live-PG tests run because
// `DATABASE_URL` is unset (every body early-returns).
#[allow(dead_code, unused_imports)]
fn _imports_used() {
    let _ = orgs_store::is_active_member;
    let _: Option<OrgId> = None;
}
