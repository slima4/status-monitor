//! Live-PG tests for the region-scoped target sources that drive the agent
//! config-pull API and the dashboard's home-region scheduler filter.

mod common;

use sqlx::PgPool;
use uptimepage::storage::AdminRepo;
use uuid::Uuid;

fn check_spec() -> serde_json::Value {
    serde_json::json!({
        "type": "http",
        "url": "https://example.com/",
        "method": "GET",
        "timeout": 5000,
        "follow_redirects": false,
        "max_redirects": 0,
        "expected_status": {"kind": "exact", "value": 200},
        "headers": {},
        "verify_tls": true,
    })
}

async fn seed_org(pool: &PgPool) -> Uuid {
    let slug = format!("rg{}", &Uuid::new_v4().simple().to_string()[..12]);
    let (org_id,): (Uuid,) =
        sqlx::query_as("INSERT INTO organizations (slug, name) VALUES ($1, 's') RETURNING id")
            .bind(&slug)
            .fetch_one(pool)
            .await
            .unwrap();
    org_id
}

async fn seed_target(pool: &PgPool, org_id: Uuid) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled) \
         VALUES ($1, 't', $2::jsonb, 60, true) RETURNING id",
    )
    .bind(org_id)
    .bind(check_spec())
    .fetch_one(pool)
    .await
    .unwrap();
    id
}

async fn ensure_region(pool: &PgPool, id: &str) {
    sqlx::query("INSERT INTO regions (id, name) VALUES ($1, $1) ON CONFLICT DO NOTHING")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

async fn assign(pool: &PgPool, target_id: Uuid, region: &str) {
    sqlx::query("INSERT INTO target_regions (target_id, region) VALUES ($1, $2)")
        .bind(target_id)
        .bind(region)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn reconcile_upserts_region_and_backfills_unassigned() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let region = "eu-reconcile";
    let org = seed_org(&pool).await;
    // Seeded directly (not via the store) so it carries no region assignment.
    let orphan = seed_target(&pool, org).await;

    let repo = AdminRepo::new(pool.clone(), None, "test");
    // Region row does not exist yet — reconcile must create it, then backfill.
    repo.reconcile_regions(region, region).await.unwrap();

    let ids: Vec<Uuid> = repo
        .list_enabled_targets_for_region(region)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, t)| t.id)
        .collect();
    assert!(
        ids.contains(&orphan),
        "reconcile must assign an unassigned target to the default region"
    );

    // Idempotent: a second run neither errors nor duplicates the assignment.
    repo.reconcile_regions(region, region).await.unwrap();
    let count = repo
        .list_enabled_targets_for_region(region)
        .await
        .unwrap()
        .into_iter()
        .filter(|(_, t)| t.id == orphan)
        .count();
    assert_eq!(count, 1, "reconcile must not duplicate assignments");
}

#[tokio::test]
#[ignore]
async fn region_source_returns_only_that_region() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    ensure_region(&pool, "eu-rgtest2").await;
    ensure_region(&pool, "eu-rgother").await;
    let org = seed_org(&pool).await;
    let eu = seed_target(&pool, org).await;
    let other = seed_target(&pool, org).await;
    assign(&pool, eu, "eu-rgtest2").await;
    assign(&pool, other, "eu-rgother").await;

    let repo = AdminRepo::new(pool.clone(), None, "test");
    let ids: Vec<Uuid> = repo
        .list_enabled_targets_for_region("eu-rgtest2")
        .await
        .unwrap()
        .into_iter()
        .map(|(_, t)| t.id)
        .collect();

    assert!(ids.contains(&eu));
    assert!(
        !ids.contains(&other),
        "another region's target must not leak into the eu pull"
    );
}

#[tokio::test]
#[ignore]
async fn pull_etag_reflects_membership_swap_at_equal_count() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    ensure_region(&pool, "eu-rgetag").await;
    let org = seed_org(&pool).await;
    let a = seed_target(&pool, org).await;
    let b = seed_target(&pool, org).await;
    assign(&pool, a, "eu-rgetag").await;

    let repo = AdminRepo::new(pool.clone(), None, "test");
    let e1 = repo.region_pull_etag("eu-rgetag").await.unwrap();
    let e1b = repo.region_pull_etag("eu-rgetag").await.unwrap();
    assert_eq!(e1, e1b, "etag stable when nothing changes");

    // Swap membership keeping the count at 1.
    sqlx::query("DELETE FROM target_regions WHERE target_id = $1 AND region = $2")
        .bind(a)
        .bind("eu-rgetag")
        .execute(&pool)
        .await
        .unwrap();
    assign(&pool, b, "eu-rgetag").await;
    let e2 = repo.region_pull_etag("eu-rgetag").await.unwrap();
    assert_ne!(e1, e2, "etag must change on a same-count membership swap");
}

#[tokio::test]
#[ignore]
async fn assigned_targets_map_carries_authoritative_org() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    ensure_region(&pool, "eu-rgtest3").await;
    let org = seed_org(&pool).await;
    let t = seed_target(&pool, org).await;
    assign(&pool, t, "eu-rgtest3").await;

    let repo = AdminRepo::new(pool.clone(), None, "test");
    let map = repo
        .assigned_targets_for_region("eu-rgtest3")
        .await
        .unwrap();

    assert_eq!(map.get(&t).map(|o| o.0), Some(org));
}

#[tokio::test]
#[ignore]
async fn set_and_read_target_regions_honours_enabled_and_org() {
    use uptimepage::domain::OrgId;
    use uptimepage::storage::{PostgresTargetStore, TargetStore};

    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    ensure_region(&pool, "eu-assign").await;
    ensure_region(&pool, "us-assign").await;
    sqlx::query("UPDATE regions SET enabled = false WHERE id = 'us-assign'")
        .execute(&pool)
        .await
        .unwrap();
    let org = seed_org(&pool).await;
    let target = seed_target(&pool, org).await;
    let store = PostgresTargetStore::from_pool(pool.clone(), None);

    // available_regions excludes a disabled region.
    let avail = store.available_regions().await.unwrap();
    assert!(avail.contains(&"eu-assign".to_string()));
    assert!(!avail.contains(&"us-assign".to_string()));

    // Assignment replaces the set and reads back.
    assert!(
        store
            .set_target_regions(OrgId(org), target, &["eu-assign".to_string()])
            .await
            .unwrap()
    );
    assert_eq!(
        store.regions_for_target(OrgId(org), target).await.unwrap(),
        Some(vec!["eu-assign".to_string()])
    );

    // Cross-org: a foreign org reading this target sees not-found, not data.
    let other = seed_org(&pool).await;
    assert_eq!(
        store.regions_for_target(OrgId(other), target).await.unwrap(),
        None
    );
    assert!(
        !store
            .set_target_regions(OrgId(other), target, &["eu-assign".to_string()])
            .await
            .unwrap()
    );
}
