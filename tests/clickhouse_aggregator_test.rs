//! ClickHouse-backed integration tests for the public-status aggregator.
//!
//! These exercise the real `clickhouse-rs` bind/deserialize path, which unit
//! tests with in-memory stores miss. Both bugs that produced
//! `STATUS_DATA_UNAVAILABLE` in dev (UUID array bind + DateTime→i64 deser)
//! would have been caught by `build_round_trips_seeded_data` below.
//!
//! Skipped by default: requires the dev compose stack.
//!
//!     docker compose -f compose.dev.yml up -d
//!     DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/monitor \
//!     CLICKHOUSE_URL=http://127.0.0.1:8123 \
//!       cargo test --test clickhouse_aggregator_test -- --ignored
//!
//! Each test purges its own prefix at the start (idempotent — recovers from
//! prior crashed runs) and deletes its seeded target at the end, even on
//! panic. ClickHouse rows are left to expire via the 90-day TTL since `ALTER
//! TABLE ... DELETE` is async + expensive and our fresh UUIDs per run
//! prevent cross-test interference.

mod common;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::FutureExt;
use sqlx::PgPool;
use uptimepage::domain::{
    CheckResult, CheckSpec, CheckStatus, ExpectedStatus, NewStatusPage, NewStatusPageComponent,
    NewTarget, OrgId, PublicComponentStatus, StatusPageId, WriteSource,
};
use uptimepage::public_status::{AggregatorConfig, OrgAggregator};
use uptimepage::storage::{
    ClickhouseResultSink, PgStatusPageStore, PostgresTargetStore, ResultSink, StatusPageStore,
    TargetStore,
};
use url::Url;
use uuid::Uuid;

use crate::common::default_http_check;

/// Publish `target_id` on a fresh enabled page and return the page id, so the
/// page-keyed aggregator has a component set to filter on.
async fn seed_page_with_target(pool: &PgPool, org: OrgId, target_id: Uuid) -> StatusPageId {
    let store = PgStatusPageStore::new(pool.clone());
    let slug = format!("aggpage{}", Uuid::now_v7().simple());
    let slug = slug[..slug.len().min(30)].to_string();
    let page = store
        .create(
            org,
            NewStatusPage {
                slug,
                name: "Agg".into(),
                enabled: true,
            },
            WriteSource::Ui,
            i64::MAX,
        )
        .await
        .expect("create page")
        .expect("within page cap");
    store
        .add_component(
            org,
            page.id,
            NewStatusPageComponent {
                target_id,
                public_name: None,
                public_description: None,
                public_group: None,
                sort_order: 0,
            },
            i64::MAX,
        )
        .await
        .expect("add component");
    page.id
}

async fn seed_org(pool: &sqlx::PgPool, prefix: &str) -> uptimepage::domain::OrgId {
    let slug = format!("{prefix}-{}", Uuid::now_v7().simple());
    let slug = &slug[..slug.len().min(30)];
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ($1, 'Agg Test') RETURNING id",
    )
    .bind(slug)
    .fetch_one(pool)
    .await
    .expect("insert agg-test org");
    uptimepage::domain::OrgId(id)
}

fn public_target(name: &str) -> NewTarget {
    let url = Url::parse("https://example.com/").unwrap();
    NewTarget {
        name: name.into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: Default::default(),
        group_name: None,
        owner_user_id: None,
    }
}

fn ok_result(target_id: Uuid, org_id: Uuid, ts: chrono::DateTime<Utc>) -> CheckResult {
    CheckResult {
        target_id,
        org_id,
        timestamp: ts,
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

/// Best-effort cleanup of leftover rows from prior runs. Scoped to a prefix
/// so parallel tests with different prefixes don't fight. Cascades to
/// incidents and maintenance_window_components via FK.
async fn purge_prefix(pool: &PgPool, prefix: &str) {
    let _ = sqlx::query("DELETE FROM targets WHERE name LIKE $1")
        .bind(format!("{prefix}%"))
        .execute(pool)
        .await;
}

async fn delete_target(pool: &PgPool, id: Uuid) {
    let _ = sqlx::query("DELETE FROM targets WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

/// Runs `f` and always cleans up `target_id` from PG, even if `f` panics.
/// Re-raises the panic afterwards so the test still fails loudly.
async fn with_cleanup<F>(pool: &PgPool, target_id: Uuid, f: F)
where
    F: std::future::Future<Output = ()>,
{
    let result = AssertUnwindSafe(f).catch_unwind().await;
    delete_target(pool, target_id).await;
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

/// Exercises both `has(?, target_id)` query sites (recent counters + history
/// strip) and the `DateTime → i64` deserialization in the history-strip
/// `SELECT`. Either bug breaks this test with a 503-equivalent error.
#[tokio::test]
#[ignore = "requires DATABASE_URL + CLICKHOUSE_URL — run via `docker compose -f compose.dev.yml up -d` then `cargo test -- --ignored`"]
async fn build_round_trips_seeded_data() {
    let Some(pool) = common::pg_pool_from_env().await else {
        eprintln!("skipped: DATABASE_URL not set");
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        eprintln!("skipped: CLICKHOUSE_URL not set");
        return;
    };

    purge_prefix(&pool, "agg-test-").await;

    let org_id = seed_org(&pool, "agg-a").await;
    let unique = format!("agg-test-{}", Uuid::now_v7());
    let store = Arc::new(PostgresTargetStore::from_pool(pool.clone(), None));
    let target = store
        .create(org_id, public_target(&unique), WriteSource::Ui, i64::MAX)
        .await
        .expect("create public target");
    let target_id = target.id;
    let pool_for_cleanup = pool.clone();

    with_cleanup(&pool_for_cleanup, target_id, async move {
        let sink = ClickhouseResultSink::from_client(ch.clone());
        let now = Utc::now();
        let rows: Vec<CheckResult> = (0..5)
            .map(|i| ok_result(target_id, org_id.0, now - chrono::Duration::seconds(i * 30)))
            .collect();
        sink.write_batch(&rows).await.expect("ch insert");

        // The materialized view rolls up on insert, but the underlying merge
        // happens asynchronously. Force a flush so the 5-minute counters
        // query sees the rows we just wrote.
        ch.query("OPTIMIZE TABLE check_results_1m FINAL")
            .execute()
            .await
            .expect("flush mv");

        let page_id = seed_page_with_target(&pool, org_id, target_id).await;
        let agg = OrgAggregator::new(pool, ch, AggregatorConfig::default());
        let (page, _markers, _names) = agg.build(page_id, org_id).await.expect("aggregator build");

        let component = page
            .groups
            .iter()
            .flat_map(|g| &g.components)
            .find(|c| c.id == target_id)
            .expect("seeded public component present in page");

        // 5 up-results in the last 5 minutes → operational.
        assert_eq!(component.current_status, PublicComponentStatus::Operational);
        assert_eq!(component.history.len(), 90);
    })
    .await;
}

/// `component_history` is a separate code path from `build()`. It hits the
/// same history-strip query directly — would have caught the DateTime→i64
/// bug independently of the page-build smoke test.
#[tokio::test]
#[ignore = "requires DATABASE_URL + CLICKHOUSE_URL — run via `docker compose -f compose.dev.yml up -d` then `cargo test -- --ignored`"]
async fn component_history_returns_strip_for_public_target() {
    let Some(pool) = common::pg_pool_from_env().await else {
        eprintln!("skipped: DATABASE_URL not set");
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        eprintln!("skipped: CLICKHOUSE_URL not set");
        return;
    };

    purge_prefix(&pool, "hist-test-").await;

    let org_id = seed_org(&pool, "agg-b").await;
    let unique = format!("hist-test-{}", Uuid::now_v7());
    let store = Arc::new(PostgresTargetStore::from_pool(pool.clone(), None));
    let target = store
        .create(org_id, public_target(&unique), WriteSource::Ui, i64::MAX)
        .await
        .expect("create public target");
    let target_id = target.id;
    let pool_for_cleanup = pool.clone();

    with_cleanup(&pool_for_cleanup, target_id, async move {
        let sink = ClickhouseResultSink::from_client(ch.clone());
        let now = Utc::now();
        sink.write_batch(&[ok_result(target_id, org_id.0, now)])
            .await
            .expect("ch insert");
        ch.query("OPTIMIZE TABLE check_results_1m FINAL")
            .execute()
            .await
            .expect("flush mv");

        let page_id = seed_page_with_target(&pool, org_id, target_id).await;
        let agg = OrgAggregator::new(pool, ch, AggregatorConfig::default());
        let resp = agg
            .component_history(page_id, org_id, target_id, 7)
            .await
            .expect("component_history succeeds");
        assert_eq!(resp.component_id, target_id);
        assert_eq!(resp.days, 7);
        assert_eq!(resp.history.len(), 7);
    })
    .await;
}

/// The visibility gate: an `internal` incident on a public component must not
/// surface in the page's active list, recent list, or history markers — only
/// `public` incidents do. Guards the internal-vs-public separation end-to-end.
#[tokio::test]
#[ignore = "requires DATABASE_URL + CLICKHOUSE_URL"]
async fn build_excludes_internal_incidents() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };
    purge_prefix(&pool, "agg-vis-").await;

    let org_id = seed_org(&pool, "agg-vis").await;
    let unique = format!("agg-vis-{}", Uuid::now_v7());
    let store = Arc::new(PostgresTargetStore::from_pool(pool.clone(), None));
    let target = store
        .create(org_id, public_target(&unique), WriteSource::Ui, i64::MAX)
        .await
        .expect("create public target");
    let target_id = target.id;
    let pool_for_cleanup = pool.clone();

    with_cleanup(&pool_for_cleanup, target_id, async move {
        let now = Utc::now();
        let mk = |vis: &str| {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO incidents (org_id, target_id, started_at, status_at_start, visibility) \
                 VALUES ($1, $2, $3, 'down', $4) RETURNING id",
            )
            .bind(org_id.0)
            .bind(target_id)
            .bind(now - chrono::Duration::minutes(5))
            .bind(vis.to_string())
        };
        let public_id = mk("public").fetch_one(&pool).await.expect("insert public incident");
        let _internal_id = mk("internal").fetch_one(&pool).await.expect("insert internal incident");

        let page_id = seed_page_with_target(&pool, org_id, target_id).await;
        let agg = OrgAggregator::new(pool, ch, AggregatorConfig::default());
        let (page, markers, _names) = agg.build(page_id, org_id).await.expect("aggregator build");

        let active: Vec<Uuid> = page.active_incidents.iter().map(|i| i.id).collect();
        assert_eq!(active, vec![public_id], "only the public incident is active");
        let recent: Vec<Uuid> = page.recent_incidents.iter().map(|i| i.id).collect();
        assert_eq!(recent, vec![public_id], "only the public incident is recent");
        let marked: Vec<Uuid> = markers.iter().map(|m| m.id).collect();
        assert_eq!(marked, vec![public_id], "only the public incident is marked");
    })
    .await;
}
