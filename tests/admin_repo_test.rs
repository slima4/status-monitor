//! Live-PG tests for `AdminRepo` — the cross-tenant lane the scheduler uses
//! to materialise its global registry. The load-bearing invariant is that
//! soft-deleted orgs disappear from this list immediately: their targets
//! must stop emitting check_results, otherwise 30 days of grace-window
//! data accumulate in ClickHouse and (worse) Slack/webhook alerts keep
//! pinging a customer who's already cancelled.

mod common;

use sqlx::PgPool;
use status_monitor::storage::AdminRepo;
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
