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
        r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled, public_status)
           VALUES ($1, 't-pub', $2::jsonb, 60, true, true)
           RETURNING id"#,
    )
    .bind(org_id)
    .bind(check_spec)
    .fetch_one(pool)
    .await
    .unwrap();
    target_id
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
///   * skip targets with `public_status = false`
///   * skip targets whose org is soft-deleted (load-bearing — same reason
///     as the scheduler walk above)
///   * never re-emit a row from a previous page
///   * order monotonically by `(org_id, id)` so the cursor is correct
#[tokio::test]
#[ignore]
async fn public_status_pagination_filters_and_orders() {
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

    // Single-page walk: only the live org's public target survives.
    let page = repo.next_public_status_page(None, 1024).await.unwrap();
    let ours: Vec<&Uuid> = page
        .iter()
        .filter(|(_, t)| t.id == public_target || t.id == private_target || t.id == dead_public)
        .map(|(_, t)| &t.id)
        .collect();
    assert_eq!(
        ours,
        vec![&public_target],
        "only live public target visible"
    );

    // Multi-page walk with page_size=1 must still terminate without
    // re-emitting `public_target`.
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor: Option<PublicTargetCursor> = None;
    let stop_after = page.len() + 4;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps <= stop_after, "pagination must terminate");
        let p = repo.next_public_status_page(cursor, 1).await.unwrap();
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
        !seen.contains(&private_target),
        "non-public target must stay hidden in the paginated walk"
    );

    // Cleanup.
    sqlx::query("DELETE FROM organizations WHERE id IN ($1, $2)")
        .bind(org_live)
        .bind(org_dead)
        .execute(&pool)
        .await
        .unwrap();
}
