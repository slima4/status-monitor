//! Storage-layer tenant-isolation suite (acceptance #6). Provisions two orgs
//! with distinct owners, fills each with the same kinds of data, then asserts
//! every per-org store backed by Postgres or ClickHouse only sees its own
//! org's rows.
//!
//! Live-PG + live-CH. Ignored at the `cargo test` default; runs under
//! `--include-ignored` once `DATABASE_URL` and `CLICKHOUSE_URL` are set.

mod common;

use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;
use status_monitor::domain::{
    CheckResult, CheckSpec, CheckStatus, ExpectedStatus, NewMaintenanceWindow, NewTarget, OrgId,
    UserId,
};
use status_monitor::storage::traits::TimeRange;
use status_monitor::storage::{
    ClickhouseResultSink, ClickhouseResultsStore, MaintenanceStore, PgMaintenanceStore,
    PostgresTargetStore, ResultSink, ResultsStore, TargetFilter, TargetStore,
    create_org_with_owner, is_active_member,
};
use url::Url;
use uuid::Uuid;

use crate::common::default_http_check;

async fn make_user(pool: &PgPool) -> UserId {
    let email = format!("isol-{}@test.example", Uuid::now_v7());
    let (id,): (Uuid,) = sqlx::query_as(r#"INSERT INTO users (email) VALUES ($1) RETURNING id"#)
        .bind(&email)
        .fetch_one(pool)
        .await
        .expect("insert user");
    UserId(id)
}

fn unique_slug(prefix: &str) -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &id[id.len() - 6..])
}

fn target_named(name: &str) -> NewTarget {
    let url = Url::parse("https://example.com/").unwrap();
    NewTarget {
        name: name.into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: Default::default(),
        public_status: false,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    }
}

fn ok_result(target_id: Uuid) -> CheckResult {
    CheckResult {
        target_id,
        timestamp: Utc::now(),
        status: CheckStatus::Up,
        duration_ms: 42,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: None,
        response_code: Some(200),
        response_size: None,
        error: None,
    }
}

fn time_range_around_now() -> TimeRange {
    let now = Utc::now();
    TimeRange {
        from: now - chrono::Duration::minutes(5),
        to: now + chrono::Duration::minutes(5),
    }
}

struct Tenant {
    user: UserId,
    org: OrgId,
    target_store: PostgresTargetStore,
    maintenance_store: PgMaintenanceStore,
    result_sink: ClickhouseResultSink,
    results_store: ClickhouseResultsStore,
}

async fn provision_tenant(pool: &PgPool, ch: &clickhouse::Client, label: &str) -> Tenant {
    let user = make_user(pool).await;
    let org = create_org_with_owner(pool, user, &unique_slug(label), label, 3)
        .await
        .unwrap()
        .expect("provision org");
    let target_store = PostgresTargetStore::from_pool(pool.clone(), None, org.id);
    let maintenance_store = PgMaintenanceStore::new(pool.clone(), org.id);
    let result_sink = ClickhouseResultSink::from_client(ch.clone(), org.id);
    let results_store = ClickhouseResultsStore::from_client(ch.clone(), org.id);
    Tenant {
        user,
        org: org.id,
        target_store,
        maintenance_store,
        result_sink,
        results_store,
    }
}

async fn teardown(pool: &PgPool, ch: &clickhouse::Client, t: &Tenant) {
    // ALTER TABLE ... DELETE on ClickHouse is async server-side but
    // idempotent. Issue it before the PG cascade so the rows are scheduled
    // for removal even if the test only lives in CH for a few minutes.
    let _ = ch
        .query("ALTER TABLE check_results DELETE WHERE org_id = ?")
        .bind(t.org.0)
        .execute()
        .await;
    let _ = ch
        .query("ALTER TABLE check_results_1m DELETE WHERE org_id = ?")
        .bind(t.org.0)
        .execute()
        .await;

    // ON DELETE CASCADE on tenant tables wipes targets, maintenance,
    // memberships, audit rows. The user row goes last; its FK to memberships
    // cascades the other direction.
    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(t.org.0)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(t.user.0)
        .execute(pool)
        .await;
}

/// Two-tenant cross-contamination matrix. Each per-tenant store is rebuilt
/// with the other tenant's org id and asserted to see zero of the first
/// tenant's data. Mirrors acceptance #6 at the storage layer — the API layer
/// pulls these stores through `AppState`, so storage-level isolation is the
/// invariant the HTTP routes rest on.
#[tokio::test]
#[ignore]
async fn two_tenants_never_see_each_others_data() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };

    let a = provision_tenant(&pool, &ch, "tenant-a").await;
    let b = provision_tenant(&pool, &ch, "tenant-b").await;

    // ── Targets ──────────────────────────────────────────────────────────
    let target_a = a
        .target_store
        .create(
            target_named(&format!("a-target-{}", Uuid::now_v7())),
            i64::MAX,
        )
        .await
        .expect("create target in a");
    let target_b = b
        .target_store
        .create(
            target_named(&format!("b-target-{}", Uuid::now_v7())),
            i64::MAX,
        )
        .await
        .expect("create target in b");

    let a_list = a.target_store.list(TargetFilter::default()).await.unwrap();
    let b_list = b.target_store.list(TargetFilter::default()).await.unwrap();
    assert!(
        a_list.iter().any(|t| t.id == target_a.id),
        "tenant a sees its own target"
    );
    assert!(
        !a_list.iter().any(|t| t.id == target_b.id),
        "tenant a must not see tenant b's target"
    );
    assert!(
        b_list.iter().any(|t| t.id == target_b.id),
        "tenant b sees its own target"
    );
    assert!(
        !b_list.iter().any(|t| t.id == target_a.id),
        "tenant b must not see tenant a's target"
    );

    // Get-by-id across tenants resolves to None.
    assert!(
        a.target_store.get(target_b.id).await.unwrap().is_none(),
        "tenant a get of b's target id must be None"
    );
    assert!(
        b.target_store.get(target_a.id).await.unwrap().is_none(),
        "tenant b get of a's target id must be None"
    );

    // ── Maintenance windows ──────────────────────────────────────────────
    let now = Utc::now();
    let mw_a = a
        .maintenance_store
        .create(NewMaintenanceWindow {
            title: "a-window".into(),
            description: None,
            starts_at: now,
            ends_at: now + chrono::Duration::hours(1),
            component_ids: vec![],
        })
        .await
        .expect("a mw");
    let mw_b = b
        .maintenance_store
        .create(NewMaintenanceWindow {
            title: "b-window".into(),
            description: None,
            starts_at: now,
            ends_at: now + chrono::Duration::hours(1),
            component_ids: vec![],
        })
        .await
        .expect("b mw");

    assert!(
        a.maintenance_store.get(mw_b.id).await.unwrap().is_none(),
        "tenant a maintenance get of b's id must be None"
    );
    assert!(
        b.maintenance_store.get(mw_a.id).await.unwrap().is_none(),
        "tenant b maintenance get of a's id must be None"
    );

    // ── ClickHouse results ───────────────────────────────────────────────
    a.result_sink
        .write_batch(&[ok_result(target_a.id)])
        .await
        .expect("ch insert a");
    b.result_sink
        .write_batch(&[ok_result(target_b.id)])
        .await
        .expect("ch insert b");

    let range = time_range_around_now();
    // A's store filters by org_a → sees a row for target_a.
    let a_results = a
        .results_store
        .list_results(target_a.id, range, 10, 0)
        .await
        .unwrap();
    assert!(
        !a_results.is_empty(),
        "tenant a results_store must see its own row"
    );
    // A queries b's target id under its own org_id → 0 rows (org_id filter
    // wins over target_id match, since CH rows tagged with org_b are
    // invisible to a's store).
    let cross = a
        .results_store
        .list_results(target_b.id, range, 10, 0)
        .await
        .unwrap();
    assert!(
        cross.is_empty(),
        "tenant a results_store must not return rows tagged with b's org"
    );
    let cross_count = a
        .results_store
        .count_results(target_b.id, range)
        .await
        .unwrap();
    assert_eq!(cross_count, 0, "count must also enforce org_id filter");

    // ── Membership ──────────────────────────────────────────────────────
    assert!(
        is_active_member(&pool, a.user, a.org).await.unwrap(),
        "owner is active in own org"
    );
    assert!(
        !is_active_member(&pool, a.user, b.org).await.unwrap(),
        "user A must not be member of org B"
    );
    assert!(
        !is_active_member(&pool, b.user, a.org).await.unwrap(),
        "user B must not be member of org A"
    );

    teardown(&pool, &ch, &a).await;
    teardown(&pool, &ch, &b).await;
}
