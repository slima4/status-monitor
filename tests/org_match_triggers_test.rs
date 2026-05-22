//! Schema-layer integration tests for the `006_org_match_triggers.up.sql`
//! belt-and-suspenders guards. These triggers refuse any tenant-child row
//! whose denormalised `org_id` doesn't match its parent's (or, for
//! `maintenance_window_components`, doesn't match its referenced target's).
//! The repository layer is supposed to enforce the same invariant from
//! Rust, but the triggers are the last line that catches a bypass — direct
//! SQL, a future repo path that forgets, a misuse via an admin tool.
//!
//! Skipped by default; runs under `--include-ignored` once `DATABASE_URL`
//! is set.

mod common;

use sqlx::PgPool;
use uuid::Uuid;

async fn seed_two_orgs(pool: &PgPool) -> (Uuid, Uuid) {
    let suffix = Uuid::new_v4().simple().to_string();
    let a: (Uuid,) =
        sqlx::query_as("INSERT INTO organizations (slug, name) VALUES ($1, 'a') RETURNING id")
            .bind(format!("orgtrig-a-{}", &suffix[..8]))
            .fetch_one(pool)
            .await
            .unwrap();
    let b: (Uuid,) =
        sqlx::query_as("INSERT INTO organizations (slug, name) VALUES ($1, 'b') RETURNING id")
            .bind(format!("orgtrig-b-{}", &suffix[..8]))
            .fetch_one(pool)
            .await
            .unwrap();
    (a.0, b.0)
}

async fn seed_target(pool: &PgPool, org: Uuid, name: &str) -> Uuid {
    let row: (Uuid,) = sqlx::query_as(
        r#"INSERT INTO targets (org_id, name, check_spec, interval_secs)
           VALUES ($1, $2, '{"type":"http","url":"https://example.com/"}'::jsonb, 60)
           RETURNING id"#,
    )
    .bind(org)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();
    row.0
}

async fn seed_incident(pool: &PgPool, org: Uuid, target: Uuid) -> Uuid {
    let row: (Uuid,) = sqlx::query_as(
        r#"INSERT INTO incidents (org_id, target_id, started_at, status_at_start)
           VALUES ($1, $2, now(), 'down')
           RETURNING id"#,
    )
    .bind(org)
    .bind(target)
    .fetch_one(pool)
    .await
    .unwrap();
    row.0
}

async fn seed_maintenance(pool: &PgPool, org: Uuid) -> Uuid {
    let row: (Uuid,) = sqlx::query_as(
        r#"INSERT INTO maintenance_windows (org_id, title, starts_at, ends_at)
           VALUES ($1, 't', now(), now() + INTERVAL '1 hour')
           RETURNING id"#,
    )
    .bind(org)
    .fetch_one(pool)
    .await
    .unwrap();
    row.0
}

async fn cleanup_orgs(pool: &PgPool, orgs: &[Uuid]) {
    let _ = sqlx::query("DELETE FROM organizations WHERE id = ANY($1)")
        .bind(orgs)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore]
async fn incident_updates_cross_org_insert_raises() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b) = seed_two_orgs(&pool).await;
    let target_a = seed_target(&pool, org_a, "ta").await;
    let incident_a = seed_incident(&pool, org_a, target_a).await;

    // Insert an incident_update tagged with org_b but pointing at org_a's
    // incident — must trip the BEFORE-INSERT trigger.
    let err = sqlx::query(
        r#"INSERT INTO incident_updates (org_id, incident_id, phase, message)
           VALUES ($1, $2, 'investigating', 'x')"#,
    )
    .bind(org_b)
    .bind(incident_a)
    .execute(&pool)
    .await
    .expect_err("cross-org incident_update must be rejected");
    assert!(
        err.to_string().contains("org_id mismatch"),
        "expected trigger exception, got: {err}"
    );

    cleanup_orgs(&pool, &[org_a, org_b]).await;
}

#[tokio::test]
#[ignore]
async fn maintenance_components_cross_org_parent_insert_raises() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b) = seed_two_orgs(&pool).await;
    let target_b = seed_target(&pool, org_b, "tb").await;
    let mw_a = seed_maintenance(&pool, org_a).await;

    // Row stamped org_b but references org_a's maintenance window — must
    // trip the parent-org trigger.
    let err = sqlx::query(
        r#"INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(org_b)
    .bind(mw_a)
    .bind(target_b)
    .execute(&pool)
    .await
    .expect_err("cross-org maintenance_window_components insert must be rejected");
    assert!(
        err.to_string().contains("org_id mismatch"),
        "expected parent-org trigger exception, got: {err}"
    );

    cleanup_orgs(&pool, &[org_a, org_b]).await;
}

#[tokio::test]
#[ignore]
async fn maintenance_components_cross_org_target_insert_raises() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b) = seed_two_orgs(&pool).await;
    let target_b = seed_target(&pool, org_b, "tb").await;
    let mw_a = seed_maintenance(&pool, org_a).await;

    // Row consistent with mw's org (a) but points at org_b's target — must
    // trip the new target-org trigger.
    let err = sqlx::query(
        r#"INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(org_a)
    .bind(mw_a)
    .bind(target_b)
    .execute(&pool)
    .await
    .expect_err("cross-org target reference must be rejected");
    assert!(
        err.to_string().contains("target_id"),
        "expected target-org trigger exception, got: {err}"
    );

    cleanup_orgs(&pool, &[org_a, org_b]).await;
}

#[tokio::test]
#[ignore]
async fn maintenance_components_target_swap_to_foreign_org_raises() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b) = seed_two_orgs(&pool).await;
    let target_a = seed_target(&pool, org_a, "ta").await;
    let target_b = seed_target(&pool, org_b, "tb").await;
    let mw_a = seed_maintenance(&pool, org_a).await;

    sqlx::query(
        r#"INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(org_a)
    .bind(mw_a)
    .bind(target_a)
    .execute(&pool)
    .await
    .unwrap();

    // Future code path that does a bare `UPDATE OF target_id` to swap to
    // another org's target. The broadened trigger now covers
    // `UPDATE OF target_id`; the target-org trigger refuses the new value.
    let err = sqlx::query(
        r#"UPDATE maintenance_window_components
              SET target_id = $1
            WHERE maintenance_id = $2"#,
    )
    .bind(target_b)
    .bind(mw_a)
    .execute(&pool)
    .await
    .expect_err("target_id swap to foreign org must be rejected");
    assert!(
        err.to_string().contains("target_id") || err.to_string().contains("org_id mismatch"),
        "expected trigger exception, got: {err}"
    );

    cleanup_orgs(&pool, &[org_a, org_b]).await;
}

#[tokio::test]
#[ignore]
async fn same_org_inserts_succeed() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, _org_b) = seed_two_orgs(&pool).await;
    let target_a = seed_target(&pool, org_a, "ta").await;
    let incident_a = seed_incident(&pool, org_a, target_a).await;
    let mw_a = seed_maintenance(&pool, org_a).await;

    sqlx::query(
        r#"INSERT INTO incident_updates (org_id, incident_id, phase, message)
           VALUES ($1, $2, 'investigating', 'baseline')"#,
    )
    .bind(org_a)
    .bind(incident_a)
    .execute(&pool)
    .await
    .expect("same-org incident_update must succeed");

    sqlx::query(
        r#"INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(org_a)
    .bind(mw_a)
    .bind(target_a)
    .execute(&pool)
    .await
    .expect("same-org maintenance_window_components must succeed");

    cleanup_orgs(&pool, &[org_a, _org_b]).await;
}

// ── public_custom_domain canonical-form CHECK ─────────────────────────

#[tokio::test]
#[ignore]
async fn public_custom_domain_rejects_uppercase() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, _b) = seed_two_orgs(&pool).await;
    let err = sqlx::query("UPDATE organizations SET public_custom_domain = $1 WHERE id = $2")
        .bind("Status.ACME.com")
        .bind(org_a)
        .execute(&pool)
        .await
        .expect_err("uppercase domain must be rejected");
    assert!(
        err.to_string().contains("public_custom_domain_canonical"),
        "expected CHECK constraint, got: {err}"
    );
    cleanup_orgs(&pool, &[org_a, _b]).await;
}

#[tokio::test]
#[ignore]
async fn public_custom_domain_rejects_trailing_dot() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, _b) = seed_two_orgs(&pool).await;
    let err = sqlx::query("UPDATE organizations SET public_custom_domain = $1 WHERE id = $2")
        .bind("status.acme.com.")
        .bind(org_a)
        .execute(&pool)
        .await
        .expect_err("trailing-dot domain must be rejected");
    assert!(
        err.to_string().contains("public_custom_domain_canonical"),
        "expected CHECK constraint, got: {err}"
    );
    cleanup_orgs(&pool, &[org_a, _b]).await;
}

#[tokio::test]
#[ignore]
async fn public_custom_domain_case_variant_collides_under_citext_unique() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let (org_a, org_b) = seed_two_orgs(&pool).await;
    // Canonical lowercase form on org_a.
    sqlx::query("UPDATE organizations SET public_custom_domain = $1 WHERE id = $2")
        .bind("status.acme.com")
        .bind(org_a)
        .execute(&pool)
        .await
        .expect("canonical insert must succeed");
    // Pre-canonicalised lowercase for org_b — must collide under CITEXT
    // UNIQUE because byte-distinct case variants would otherwise both
    // verify. Even though the CHECK forces lowercase, the UNIQUE behaviour
    // is the load-bearing part — pin it directly with two canonical rows.
    let err = sqlx::query("UPDATE organizations SET public_custom_domain = $1 WHERE id = $2")
        .bind("status.acme.com")
        .bind(org_b)
        .execute(&pool)
        .await
        .expect_err("duplicate domain on second org must be rejected by UNIQUE");
    let m = err.to_string();
    assert!(
        m.contains("unique") || m.contains("duplicate") || m.contains("public_custom_domain"),
        "expected UNIQUE violation, got: {err}"
    );
    cleanup_orgs(&pool, &[org_a, org_b]).await;
}
