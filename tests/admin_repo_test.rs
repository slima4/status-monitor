//! Live-PG tests for `AdminRepo` — the cross-tenant lane the scheduler uses
//! to materialise its global registry. The load-bearing invariant is that
//! soft-deleted orgs disappear from this list immediately: their targets
//! must stop emitting check_results, otherwise 30 days of grace-window
//! data accumulate in ClickHouse and (worse) Slack/webhook alerts keep
//! pinging a customer who's already cancelled.

mod common;

use sqlx::PgPool;
use uptimepage::storage::AdminRepo;
use uptimepage::storage::admin::PublicTargetCursor;
use uuid::Uuid;

async fn seed_org_with_target(pool: &PgPool, slug: &str) -> (Uuid, Uuid) {
    let (org_id,): (Uuid,) =
        sqlx::query_as("INSERT INTO organizations (slug, name) VALUES ($1, 's') RETURNING id")
            .bind(slug)
            .fetch_one(pool)
            .await
            .unwrap();
    let check_spec = serde_json::json!({
        "type": "http",
        "url": "https://example.com/",
        "method": "GET",
        "timeout": 5000,
        "follow_redirects": false,
        "max_redirects": 0,
        "expected_status": {"kind": "exact", "value": 200},
        "headers": {},
        "verify_tls": true,
    });
    let (target_id,): (Uuid,) = sqlx::query_as(
        r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled)
           VALUES ($1, 't', $2::jsonb, 60, true)
           RETURNING id"#,
    )
    .bind(org_id)
    .bind(check_spec)
    .fetch_one(pool)
    .await
    .unwrap();
    (org_id, target_id)
}

async fn seed_public_target(pool: &PgPool, org_id: Uuid) -> Uuid {
    let check_spec = serde_json::json!({
        "type": "http",
        "url": "https://example.com/",
        "method": "GET",
        "timeout": 5000,
        "follow_redirects": false,
        "max_redirects": 0,
        "expected_status": {"kind": "exact", "value": 200},
        "headers": {},
        "verify_tls": true,
    });
    let (target_id,): (Uuid,) = sqlx::query_as(
        r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled)
           VALUES ($1, 't-pub', $2::jsonb, 60, true)
           RETURNING id"#,
    )
    .bind(org_id)
    .bind(check_spec)
    .fetch_one(pool)
    .await
    .unwrap();
    bind_to_new_page(pool, org_id, target_id).await;
    target_id
}

/// Put `target_id` on a brand-new enabled page for `org_id`. Publicness is a
/// `status_page_components` binding now, not a `targets` column; binding a
/// target to several pages exercises the writer-walk's `DISTINCT ON`.
async fn bind_to_new_page(pool: &PgPool, org_id: Uuid, target_id: Uuid) {
    let slug = format!("admpg{}", &Uuid::new_v4().simple().to_string()[..16]);
    let (page_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO status_pages (org_id, slug, name, enabled) \
         VALUES ($1, $2, 'p', true) RETURNING id",
    )
    .bind(org_id)
    .bind(&slug)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO status_page_components (org_id, status_page_id, target_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(org_id)
    .bind(page_id)
    .bind(target_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn enabled_targets_excludes_soft_deleted_orgs() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let slug = format!("admrep-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (org_id, target_id) = seed_org_with_target(&pool, &slug).await;

    let repo = AdminRepo::new(pool.clone(), None, "test");
    let before = repo.list_all_enabled_targets().await.unwrap();
    assert!(
        before.iter().any(|(_, t)| t.id == target_id),
        "live org's target must be visible to the scheduler"
    );

    // Soft-delete the org. Per the SaaS contract, the scheduler must stop
    // seeing this org's targets *immediately* — no grace-window write
    // traffic, no post-cancellation alerts.
    sqlx::query("UPDATE organizations SET deleted_at = now() WHERE id = $1")
        .bind(org_id)
        .execute(&pool)
        .await
        .unwrap();

    let after = repo.list_all_enabled_targets().await.unwrap();
    assert!(
        !after.iter().any(|(_, t)| t.id == target_id),
        "soft-deleted org's target must NOT remain in the scheduler's enabled set"
    );

    // Cleanup.
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org_id)
        .execute(&pool)
        .await
        .unwrap();
}

/// Keyset paginator must:
///   * include every enabled target — public and private alike
///   * skip targets whose org is soft-deleted (load-bearing — same reason
///     as the scheduler walk above)
///   * never re-emit a row from a previous page
///   * order monotonically by `(org_id, id)` so the cursor is correct
#[tokio::test]
#[ignore]
async fn enabled_target_pagination_includes_all_and_skips_dead_org() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let suffix = &Uuid::new_v4().simple().to_string()[..8];
    let slug_live = format!("pubpg-{suffix}-l");
    let slug_dead = format!("pubpg-{suffix}-d");

    // Live org with one public + one private target.
    let (org_live, private_target) = seed_org_with_target(&pool, &slug_live).await;
    let public_target = seed_public_target(&pool, org_live).await;
    // Dead org with a public target — must be skipped.
    let (org_dead, _) = seed_org_with_target(&pool, &slug_dead).await;
    let dead_public = seed_public_target(&pool, org_dead).await;
    sqlx::query("UPDATE organizations SET deleted_at = now() WHERE id = $1")
        .bind(org_dead)
        .execute(&pool)
        .await
        .unwrap();

    let repo = AdminRepo::new(pool.clone(), None, "test");

    // Single-page walk: both of the live org's targets survive (public and
    // private); the dead org's target is filtered out.
    let page = repo.next_enabled_target_page(None, 1024).await.unwrap();
    let mut ours: Vec<Uuid> = page
        .iter()
        .filter(|(_, t)| t.id == public_target || t.id == private_target || t.id == dead_public)
        .map(|(_, t)| t.id)
        .collect();
    ours.sort();
    let mut want = vec![public_target, private_target];
    want.sort();
    assert_eq!(ours, want, "both live targets visible, dead org filtered");

    // Multi-page walk with page_size=1 must still terminate without
    // re-emitting `public_target`.
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<PublicTargetCursor> = None;
    let stop_after = page.len() + 4;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps <= stop_after, "pagination must terminate");
        let p = repo.next_enabled_target_page(cursor, 1).await.unwrap();
        if p.is_empty() {
            break;
        }
        let (org, t) = p.into_iter().next().unwrap();
        seen.push(t.id);
        cursor = Some(PublicTargetCursor {
            org_id: org,
            target_id: t.id,
        });
    }
    assert!(
        seen.contains(&public_target),
        "paginated walk must include the live public target"
    );
    let dupes = seen.iter().filter(|id| **id == public_target).count();
    assert_eq!(dupes, 1, "cursor must not re-emit the same row");
    assert!(
        !seen.contains(&dead_public),
        "soft-deleted org's target must stay hidden in the paginated walk"
    );
    assert!(
        seen.contains(&private_target),
        "private target now opens incidents too — must appear in the walk"
    );

    // Cleanup.
    sqlx::query("DELETE FROM organizations WHERE id IN ($1, $2)")
        .bind(org_live)
        .bind(org_dead)
        .execute(&pool)
        .await
        .unwrap();
}

/// A target bound to several enabled pages is still one `(org, target)` row in
/// the writer walk — the walk reads each `targets` row directly, so page
/// membership never fans a monitor into duplicate incident work.
#[tokio::test]
#[ignore]
async fn target_on_multiple_pages_emitted_once() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let suffix = &Uuid::new_v4().simple().to_string()[..8];
    let (org, _private) = seed_org_with_target(&pool, &format!("multi-{suffix}")).await;
    // seed_public_target binds onto one page; add a second enabled page for it.
    let target = seed_public_target(&pool, org).await;
    bind_to_new_page(&pool, org, target).await;

    let repo = AdminRepo::new(pool.clone(), None, "test");
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<PublicTargetCursor> = None;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps <= 1000, "pagination must terminate");
        let p = repo.next_enabled_target_page(cursor, 1).await.unwrap();
        if p.is_empty() {
            break;
        }
        let (o, t) = p.into_iter().next().unwrap();
        seen.push(t.id);
        cursor = Some(PublicTargetCursor {
            org_id: o,
            target_id: t.id,
        });
    }
    assert_eq!(
        seen.iter().filter(|id| **id == target).count(),
        1,
        "target on two enabled pages must appear exactly once in the walk"
    );

    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
}
