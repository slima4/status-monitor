//! Postgres-backed regression: `confirmed_downtime_by_target` must skip
//! target-less (manual) incidents. Its `GROUP BY target_id` would otherwise
//! return a NULL row and fail decoding into `Uuid`. The in-memory store can't
//! reproduce this — its `Incident.target_id` is non-nullable.
//!
//! `#[ignore]`d by default; runs under `--run-ignored all` with `DATABASE_URL`
//! set. The harness auto-applies migrations on first connect.

mod common;

use common::{make_user, unique_slug};
use sqlx::PgPool;
use uptimepage::domain::OrgId;
use uptimepage::storage::{
    IncidentNarrationStore, PgIncidentNarrationStore, TimeRange, create_org_with_owner,
};
use uuid::Uuid;

async fn seed(pool: &PgPool, prefix: &str) -> (OrgId, Uuid) {
    let user = make_user(pool, prefix).await;
    let org = create_org_with_owner(pool, user, &unique_slug(prefix), "svc", 3)
        .await
        .expect("create org")
        .expect("org created");
    let target_id: Uuid = sqlx::query_scalar(
        "INSERT INTO targets (org_id, name, check_spec, interval_secs) \
         VALUES ($1, 'svc', '{}'::jsonb, 30) RETURNING id",
    )
    .bind(org.id.0)
    .fetch_one(pool)
    .await
    .expect("insert target");
    (org.id, target_id)
}

#[tokio::test]
#[ignore]
async fn confirmed_downtime_skips_null_target_incidents_pg() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org, target_id) = seed(&pool, "cdt").await;

    // Resolved incident on the target: 10 minutes inside the window.
    sqlx::query(
        "INSERT INTO incidents (org_id, target_id, started_at, ended_at, status_at_start, \
                                check_count, state, visibility, origin) \
         VALUES ($1, $2, now() - interval '50 minute', now() - interval '40 minute', \
                 'down', 1, 'resolved', 'public', 'monitor')",
    )
    .bind(org.0)
    .bind(target_id)
    .execute(&pool)
    .await
    .expect("insert target incident");

    // Target-less manual incident in the same window — must be ignored.
    sqlx::query(
        "INSERT INTO incidents (org_id, target_id, started_at, status_at_start, \
                                check_count, state, visibility, origin) \
         VALUES ($1, NULL, now() - interval '30 minute', 'down', 1, 'triggered', \
                 'internal', 'manual')",
    )
    .bind(org.0)
    .execute(&pool)
    .await
    .expect("insert manual incident");

    let store = PgIncidentNarrationStore::new(pool.clone());
    let now = chrono::Utc::now();
    let range = TimeRange {
        from: now - chrono::Duration::hours(1),
        to: now,
    };
    let map = store
        .confirmed_downtime_by_target(org, range)
        .await
        .expect("must not error on a null-target incident");

    assert_eq!(map.len(), 1, "only the real target appears");
    assert_eq!(
        map.get(&target_id).copied(),
        Some(600),
        "10 minutes of clamped downtime"
    );
}
