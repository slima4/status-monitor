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

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use status_monitor::domain::{
    CheckResult, CheckSpec, CheckStatus, ExpectedStatus, NewTarget, PublicComponentStatus,
};
use status_monitor::public_status::{AggregatorConfig, LiveAggregator};
use status_monitor::storage::{
    ClickhouseResultSink, PostgresTargetStore, ResultSink, TargetStore,
};
use url::Url;
use uuid::Uuid;

use crate::common::default_http_check;

fn public_target(name: &str) -> NewTarget {
    let url = Url::parse("https://example.com/").unwrap();
    NewTarget {
        name: name.into(),
        check: CheckSpec::Http(default_http_check(url, ExpectedStatus::Exact(200))),
        interval: Duration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: Default::default(),
        public_status: true,
        public_name: None,
        public_description: None,
        public_group: None,
        public_sort_order: 0,
    }
}

fn ok_result(target_id: Uuid, ts: chrono::DateTime<Utc>) -> CheckResult {
    CheckResult {
        target_id,
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

    let unique = format!("agg-test-{}", Uuid::now_v7());
    let store = Arc::new(PostgresTargetStore::from_pool(pool.clone(), None));
    let target = store
        .create(public_target(&unique))
        .await
        .expect("create public target");

    let sink = ClickhouseResultSink::from_client(ch.clone());
    let now = Utc::now();
    let rows: Vec<CheckResult> = (0..5)
        .map(|i| ok_result(target.id, now - chrono::Duration::seconds(i * 30)))
        .collect();
    sink.write_batch(&rows).await.expect("ch insert");

    // The materialized view rolls up on insert, but the underlying merge
    // happens asynchronously. Force a flush so the 5-minute counters query
    // sees the rows we just wrote.
    ch.query("OPTIMIZE TABLE check_results_1m FINAL")
        .execute()
        .await
        .expect("flush mv");

    let agg = LiveAggregator::new(
        pool,
        ch,
        store.clone() as Arc<dyn TargetStore>,
        AggregatorConfig::default(),
    );
    let page = agg.build().await.expect("aggregator build");

    let component = page
        .groups
        .iter()
        .flat_map(|g| &g.components)
        .find(|c| c.id == target.id)
        .expect("seeded public component present in page");

    // 5 up-results in the last 5 minutes → operational.
    assert_eq!(component.current_status, PublicComponentStatus::Operational);
    // History strip is sized to `cfg.history_days` (default 90) regardless of
    // whether any day saw traffic — but the most-recent day must reflect data.
    assert_eq!(component.history.len(), 90);
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

    let unique = format!("hist-test-{}", Uuid::now_v7());
    let store = Arc::new(PostgresTargetStore::from_pool(pool.clone(), None));
    let target = store
        .create(public_target(&unique))
        .await
        .expect("create public target");

    let sink = ClickhouseResultSink::from_client(ch.clone());
    let now = Utc::now();
    sink.write_batch(&[ok_result(target.id, now)])
        .await
        .expect("ch insert");
    ch.query("OPTIMIZE TABLE check_results_1m FINAL")
        .execute()
        .await
        .expect("flush mv");

    let agg = LiveAggregator::new(
        pool,
        ch,
        store as Arc<dyn TargetStore>,
        AggregatorConfig::default(),
    );
    let resp = agg
        .component_history(target.id, 7)
        .await
        .expect("component_history succeeds");
    assert_eq!(resp.component_id, target.id);
    assert_eq!(resp.days, 7);
    assert_eq!(resp.history.len(), 7);
}
