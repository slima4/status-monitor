//! Live-Postgres tests for the org-management storage layer. Skipped at the
//! `cargo test` default; runs under `--include-ignored` once `DATABASE_URL` is
//! set. The tests share a database with the rest of the live-PG suite, so
//! every test seeds its own users + orgs with `Uuid::now_v7()` slugs and
//! cleans up via `ON DELETE CASCADE` from a final `DELETE FROM users`.

mod common;

use common::{make_user, unique_slug};
use status_monitor::domain::{OrgId, Role};
use status_monitor::storage::orgs as orgs_store;
use status_monitor::storage::{
    RemoveOutcome, RestoreOutcome, UpdateOrgOutcome, create_org_with_owner, ensure_default_org,
    is_active_member, is_owner, list_deleted_orgs_deleted_by, list_members, list_orgs_for_user,
    owner_org_count, personal_org_for_user, remove_member, restore_org, slug_is_available,
    soft_delete_org, update_org_fields,
};
use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn create_org_and_slug_check_round_trip() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "orgs").await;
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
    let user = make_user(&pool, "orgs").await;
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
    let user = make_user(&pool, "orgs").await;
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

    let renamed = match update_org_fields(&pool, org.id, user, Some("New name"), None)
        .await
        .unwrap()
    {
        UpdateOrgOutcome::Updated(o) => o,
        other => panic!("expected Updated, got {other:?}"),
    };
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
    let user = make_user(&pool, "orgs").await;
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
    let user = make_user(&pool, "orgs").await;
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
    let user = make_user(&pool, "orgs").await;
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
    let a = make_user(&pool, "orgs").await;
    let b = make_user(&pool, "orgs").await;
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
    let user = make_user(&pool, "orgs").await;
    assert!(personal_org_for_user(&pool, user).await.unwrap().is_none());

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

/// 50 simultaneous create-org attempts for one user with `owner_limit = 3`
/// must produce exactly 3 winners. The cap is enforced by the count-subquery
/// inside the membership INSERT, so MVCC row-level locking on
/// `(user_id, org_id)` PK serialises the writes — no row needs to know about
/// any other attempt's outcome.
#[tokio::test]
#[ignore]
async fn owner_limit_holds_under_50_concurrent_creates() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "orgs").await;

    let limit: u32 = 3;
    let attempts: usize = 50;
    let mut tasks = Vec::with_capacity(attempts);
    for i in 0..attempts {
        let pool = pool.clone();
        let slug = unique_slug(&format!("race-{i:02}"));
        tasks.push(tokio::spawn(async move {
            create_org_with_owner(&pool, user, &slug, "n", limit).await
        }));
    }

    let mut created = 0u32;
    let mut over_limit = 0u32;
    for t in tasks {
        match t.await.unwrap() {
            Ok(Some(_)) => created += 1,
            Ok(None) => {} // slug collision shouldn't happen with unique slugs
            Err(e) if format!("{e:?}").contains("OWNER_ORG_LIMIT") => over_limit += 1,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    assert_eq!(created, limit, "exactly `limit` should win");
    assert_eq!(
        over_limit,
        u32::try_from(attempts).unwrap() - limit,
        "every loser should report OWNER_ORG_LIMIT"
    );
    assert_eq!(owner_org_count(&pool, user).await.unwrap(), limit);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

/// Models the signup retry loop: user_a holds the generated slug X, user_b
/// then collides on X and must succeed on the next `generate_personal_slug`
/// draw. Uses an explicit fixed slug for user_a instead of reseeding
/// `fastrand`'s thread-local RNG — seed pollution would leak into sibling
/// tests sharing the test binary's threads.
#[tokio::test]
#[ignore]
async fn personal_org_collision_retry_succeeds() {
    use status_monitor::domain::generate_personal_slug;

    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user_a = make_user(&pool, "orgs").await;
    let user_b = make_user(&pool, "orgs").await;

    // Fixed slug shaped like a real personal slug, distinct enough that two
    // concurrent test runs won't collide.
    let suffix = &Uuid::new_v4().simple().to_string()[..6];
    let collided = format!("personal-fixed-test-{suffix}");
    let _ = create_org_with_owner(&pool, user_a, &collided, "A", 3)
        .await
        .unwrap()
        .expect("first user takes the slug");

    // user_b tries the same slug and collides.
    let none = create_org_with_owner(&pool, user_b, &collided, "B", 3)
        .await
        .unwrap();
    assert!(none.is_none(), "expected slug-collision None");

    // Retry: a fresh draw from the generator yields a distinct slug user_b
    // can claim. Five attempts mirror the signup transaction's retry budget.
    let mut succeeded = false;
    for _ in 0..5 {
        let retry_slug = generate_personal_slug();
        if retry_slug == collided {
            continue;
        }
        if let Some(o) = create_org_with_owner(&pool, user_b, &retry_slug, "B", 3)
            .await
            .unwrap()
        {
            assert_eq!(o.slug, retry_slug.to_ascii_lowercase());
            succeeded = true;
            break;
        }
    }
    assert!(succeeded, "retry should win within 5 attempts");

    for u in [user_a, user_b] {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(u.0)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore]
async fn update_org_slug_happy_path_records_audit() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "slug").await;
    let from = unique_slug("from");
    let org = create_org_with_owner(&pool, user, &from, "Co", 3)
        .await
        .unwrap()
        .unwrap();
    let to = unique_slug("to");

    let outcome = update_org_fields(&pool, org.id, user, None, Some(&to))
        .await
        .unwrap();
    let renamed = match outcome {
        UpdateOrgOutcome::Updated(o) => o,
        other => panic!("expected Updated, got {other:?}"),
    };
    assert_eq!(renamed.slug, to);
    assert_eq!(renamed.id, org.id);
    assert!(
        slug_is_available(&pool, &from).await.unwrap(),
        "old slug freed"
    );
    assert!(!slug_is_available(&pool, &to).await.unwrap());

    let (action, meta): (String, serde_json::Value) = sqlx::query_as(
        "SELECT action, metadata FROM org_audit_log \
         WHERE org_id = $1 AND action = 'org.slug_changed' \
         ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind(org.id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(action, "org.slug_changed");
    assert_eq!(meta["from"], from);
    assert_eq!(meta["to"], to);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn update_org_slug_same_slug_is_noop_updated() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "slug").await;
    let slug = unique_slug("same");
    let org = create_org_with_owner(&pool, user, &slug, "Co", 3)
        .await
        .unwrap()
        .unwrap();

    let outcome = update_org_fields(&pool, org.id, user, None, Some(&slug))
        .await
        .unwrap();
    assert!(matches!(outcome, UpdateOrgOutcome::Updated(o) if o.slug == slug));

    // No `org.slug_changed` audit row written for a no-op (combined fn only
    // audits actually-changed fields).
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM org_audit_log WHERE org_id = $1 AND action = 'org.slug_changed'",
    )
    .bind(org.id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "no-op slug must not write audit");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn update_org_slug_conflict_returns_slug_taken() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "slug").await;
    let mine = unique_slug("mine");
    let taken = unique_slug("taken");
    create_org_with_owner(&pool, user, &taken, "T", 3)
        .await
        .unwrap()
        .unwrap();
    let org = create_org_with_owner(&pool, user, &mine, "M", 3)
        .await
        .unwrap()
        .unwrap();

    let outcome = update_org_fields(&pool, org.id, user, None, Some(&taken))
        .await
        .unwrap();
    assert!(matches!(outcome, UpdateOrgOutcome::SlugTaken));
    // Original row's slug is unchanged.
    let still = orgs_store::get_org(&pool, org.id).await.unwrap().unwrap();
    assert_eq!(still.slug, mine);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn update_org_slug_not_found_on_missing_or_soft_deleted() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "slug").await;
    let target = unique_slug("x");
    // Missing org.
    let outcome = update_org_fields(&pool, OrgId(Uuid::now_v7()), user, None, Some(&target))
        .await
        .unwrap();
    assert!(matches!(outcome, UpdateOrgOutcome::NotFound));

    // Soft-deleted org.
    let org = create_org_with_owner(&pool, user, &unique_slug("del"), "D", 3)
        .await
        .unwrap()
        .unwrap();
    assert!(soft_delete_org(&pool, org.id, user).await.unwrap());
    let after = unique_slug("after");
    let outcome = update_org_fields(&pool, org.id, user, None, Some(&after))
        .await
        .unwrap();
    assert!(matches!(outcome, UpdateOrgOutcome::NotFound));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn update_org_fields_combined_name_and_slug_is_atomic() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let user = make_user(&pool, "slug").await;
    let from = unique_slug("comb");
    let org = create_org_with_owner(&pool, user, &from, "Old name", 3)
        .await
        .unwrap()
        .unwrap();
    let to = unique_slug("comb-new");

    let outcome = update_org_fields(&pool, org.id, user, Some("New name"), Some(&to))
        .await
        .unwrap();
    let updated = match outcome {
        UpdateOrgOutcome::Updated(o) => o,
        other => panic!("expected Updated, got {other:?}"),
    };
    assert_eq!(updated.name, "New name");
    assert_eq!(updated.slug, to);

    // Both audit rows written in the same tx.
    let actions: Vec<(String,)> = sqlx::query_as(
        "SELECT action FROM org_audit_log WHERE org_id = $1 AND action IN ('org.slug_changed', 'org.renamed')",
    )
    .bind(org.id.0)
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: std::collections::HashSet<_> = actions.into_iter().map(|(a,)| a).collect();
    assert!(names.contains("org.slug_changed"));
    assert!(names.contains("org.renamed"));

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
