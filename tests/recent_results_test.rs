//! ClickHouse integration test for the batched multi-target results read that
//! backs the incident writer. Exercises the real `target_id IN (...)` query,
//! the per-`(target, region)` cap, region tagging, and the `(org, target)`
//! pair filter.
//!
//! Skipped by default. Requires a ClickHouse the migrations have run against:
//!
//!     CLICKHOUSE_URL=http://127.0.0.1:8123 \
//!       cargo test --test recent_results_test -- --ignored --nocapture

mod common;

use chrono::{Duration, Utc};
use uptimepage::domain::{CheckResult, CheckStatus, OrgId};
use uptimepage::storage::{
    ClampedRange, ClickhouseResultSink, ClickhouseResultsStore, OrgTtlDays, ResultSink,
    ResultsStore, TimeRange,
};
use uuid::Uuid;

fn result(target: Uuid, org: Uuid, ts: chrono::DateTime<Utc>) -> CheckResult {
    CheckResult {
        target_id: target,
        org_id: org,
        timestamp: ts,
        status: CheckStatus::Up,
        duration_ms: 10,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: None,
        response_code: Some(200),
        response_size: None,
        error: None,
    }
}

#[tokio::test]
#[ignore = "requires ClickHouse (CLICKHOUSE_URL)"]
async fn batched_read_caps_per_target_region_and_filters_pairs() {
    let Some(ch) = common::ch_client_from_env().await else {
        eprintln!("skipped: CLICKHOUSE_URL not set");
        return;
    };

    let sink_eu = ClickhouseResultSink::new(
        ch.clone(),
        "eu".into(),
        "agent-eu".into(),
        OrgTtlDays::new(),
    );
    let sink_us = ClickhouseResultSink::new(
        ch.clone(),
        "us".into(),
        "agent-us".into(),
        OrgTtlDays::new(),
    );
    let store = ClickhouseResultsStore::from_client(ch);

    let org = Uuid::now_v7();
    let t1 = Uuid::now_v7();
    let t2 = Uuid::now_v7();
    let now = Utc::now();
    let ago = |s: i64| now - Duration::seconds(s);

    // t1: 4 eu results + 1 us result. t2: 2 eu results.
    sink_eu
        .write_batch(&[
            result(t1, org, ago(90)),
            result(t1, org, ago(60)),
            result(t1, org, ago(30)),
            result(t1, org, ago(10)),
            result(t2, org, ago(40)),
            result(t2, org, ago(20)),
        ])
        .await
        .expect("seed eu");
    sink_us
        .write_batch(&[result(t1, org, ago(15))])
        .await
        .expect("seed us");

    let range = ClampedRange::unclamped(TimeRange {
        from: ago(3600),
        to: now + Duration::seconds(60),
    });

    // One query for both targets, generous cap → every seeded row, region-tagged.
    let all = store
        .recent_results_for_targets(&[(OrgId(org), t1), (OrgId(org), t2)], range, 100)
        .await
        .expect("batched read");
    let t1_rows: Vec<_> = all.iter().filter(|(_, r)| r.target_id == t1).collect();
    let t2_rows: Vec<_> = all.iter().filter(|(_, r)| r.target_id == t2).collect();
    assert_eq!(t1_rows.len(), 5, "t1: 4 eu + 1 us");
    assert_eq!(t2_rows.len(), 2, "t2: 2 eu");
    assert!(
        t1_rows.iter().any(|(region, _)| region == "eu")
            && t1_rows.iter().any(|(region, _)| region == "us"),
        "t1 carries both region tags"
    );

    // per-target cap = 2 keeps the 2 newest per (target, region).
    let capped = store
        .recent_results_for_targets(&[(OrgId(org), t1)], range, 2)
        .await
        .expect("capped read");
    let mut eu_ts: Vec<i64> = capped
        .iter()
        .filter(|(region, r)| region == "eu" && r.target_id == t1)
        .map(|(_, r)| r.timestamp.timestamp())
        .collect();
    eu_ts.sort_unstable();
    assert_eq!(
        eu_ts,
        vec![ago(30).timestamp(), ago(10).timestamp()],
        "newest 2 eu rows for t1"
    );
    assert_eq!(
        capped
            .iter()
            .filter(|(region, r)| region == "us" && r.target_id == t1)
            .count(),
        1,
        "us has one row, under the cap"
    );

    // Wrong org for t1 → pair filter yields nothing.
    let foreign = store
        .recent_results_for_targets(&[(OrgId(Uuid::now_v7()), t1)], range, 100)
        .await
        .expect("foreign read");
    assert!(
        foreign.is_empty(),
        "mismatched (org, target) pair returns no rows"
    );

    // Empty input is a no-op, not an error.
    let empty = store
        .recent_results_for_targets(&[], range, 100)
        .await
        .expect("empty read");
    assert!(empty.is_empty());
}

#[tokio::test]
#[ignore = "requires ClickHouse (CLICKHOUSE_URL)"]
async fn list_failures_by_region_drops_up_and_scopes_region() {
    let Some(ch) = common::ch_client_from_env().await else {
        eprintln!("skipped: CLICKHOUSE_URL not set");
        return;
    };
    let sink_eu = ClickhouseResultSink::new(
        ch.clone(),
        "eu".into(),
        "agent-eu".into(),
        OrgTtlDays::new(),
    );
    let sink_us = ClickhouseResultSink::new(
        ch.clone(),
        "us".into(),
        "agent-us".into(),
        OrgTtlDays::new(),
    );
    let store = ClickhouseResultsStore::from_client(ch);

    let org = Uuid::now_v7();
    let t = Uuid::now_v7();
    let now = Utc::now();
    let ago = |s: i64| now - Duration::seconds(s);
    let with = |ts, status| CheckResult {
        status,
        ..result(t, org, ts)
    };

    // eu: 2 up + 1 down + 1 error. us: 1 up + 1 degraded.
    sink_eu
        .write_batch(&[
            with(ago(90), CheckStatus::Up),
            with(ago(75), CheckStatus::Down),
            with(ago(60), CheckStatus::Up),
            with(ago(45), CheckStatus::Error),
        ])
        .await
        .expect("seed eu");
    sink_us
        .write_batch(&[
            with(ago(50), CheckStatus::Up),
            with(ago(20), CheckStatus::Degraded),
        ])
        .await
        .expect("seed us");

    let range = ClampedRange::unclamped(TimeRange {
        from: ago(3600),
        to: now + Duration::seconds(60),
    });

    // All regions: only the three non-Up rows, region-tagged, newest first.
    let all = store
        .list_failures_by_region(OrgId(org), t, range, 100, 0, None)
        .await
        .expect("all failures");
    assert_eq!(all.len(), 3, "2 eu failures + 1 us failure, no Up rows");
    assert!(all.iter().all(|(_, r)| r.status != CheckStatus::Up));
    assert_eq!(
        all.first().map(|(_, r)| r.status),
        Some(CheckStatus::Degraded),
        "newest failure first (us, 20s ago)"
    );
    assert!(
        all.iter().any(|(region, _)| region == "eu")
            && all.iter().any(|(region, _)| region == "us"),
        "failures carry their region tag"
    );

    // Region filter scopes to one region's failures.
    let eu = store
        .list_failures_by_region(OrgId(org), t, range, 100, 0, Some("eu"))
        .await
        .expect("eu failures");
    assert_eq!(eu.len(), 2, "only eu's down + error");
    assert!(
        eu.iter()
            .all(|(region, r)| region == "eu" && r.status != CheckStatus::Up)
    );

    // Pagination covers the set without duplicating across the boundary.
    let p1 = store
        .list_failures_by_region(OrgId(org), t, range, 2, 0, None)
        .await
        .expect("page 1");
    let p2 = store
        .list_failures_by_region(OrgId(org), t, range, 2, 2, None)
        .await
        .expect("page 2");
    assert_eq!(p1.len(), 2);
    assert_eq!(p2.len(), 1);
}
