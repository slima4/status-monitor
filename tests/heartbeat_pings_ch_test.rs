//! Storage contract for the heartbeat ping log: every signal is kept whether or
//! not it changed the verdict, and the job's own output is a separate retention
//! window from the row that carries it.

mod common;

use clickhouse::Row;
use serde::Deserialize;
use uuid::Uuid;

use uptimepage::domain::{HeartbeatPingRecord, PingSignal};
use uptimepage::storage::ClickhouseHeartbeatPingSink;
use uptimepage::storage::traits::{HeartbeatPingSink, ResultsStore};

#[derive(Debug, Row, Deserialize)]
struct StoredPing {
    signal: i8,
    exit_code: Option<u8>,
    duration_ms: Option<u32>,
    body: String,
}

async fn fetch(client: &clickhouse::Client, org_id: Uuid) -> Vec<StoredPing> {
    client
        .query(
            "SELECT signal, exit_code, duration_ms, body FROM heartbeat_pings \
             WHERE org_id = ? ORDER BY received_at, signal",
        )
        .bind(org_id)
        .fetch_all::<StoredPing>()
        .await
        .expect("read heartbeat_pings")
}

fn ping(org_id: Uuid, target_id: Uuid, signal: PingSignal) -> HeartbeatPingRecord {
    HeartbeatPingRecord {
        org_id,
        target_id,
        received_at: chrono::Utc::now(),
        signal,
        exit_code: None,
        duration_ms: None,
        body: String::new(),
    }
}

#[tokio::test]
#[ignore]
async fn a_run_stores_its_start_its_outcome_and_its_output() {
    let Some(client) = common::ch_client_from_env().await else {
        eprintln!("CLICKHOUSE_URL unset; skipping");
        return;
    };
    let org_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let sink =
        ClickhouseHeartbeatPingSink::new(client.clone(), uptimepage::storage::OrgTtlDays::new());

    sink.write_ping(&ping(org_id, target_id, PingSignal::Start))
        .await;
    sink.write_ping(&HeartbeatPingRecord {
        exit_code: Some(137),
        duration_ms: Some(94_000),
        body: "rsync: connection unexpectedly closed".into(),
        ..ping(org_id, target_id, PingSignal::Fail)
    })
    .await;

    let rows = fetch(&client, org_id).await;
    assert_eq!(
        rows.len(),
        2,
        "both signals are history, not just the verdict"
    );

    let start = &rows[0];
    assert_eq!(start.signal, PingSignal::Start.as_enum8());
    assert_eq!(start.duration_ms, None, "a start times nothing");
    assert_eq!(start.exit_code, None);

    let fail = &rows[1];
    assert_eq!(fail.signal, PingSignal::Fail.as_enum8());
    assert_eq!(fail.exit_code, Some(137));
    assert_eq!(fail.duration_ms, Some(94_000));
    assert!(fail.body.contains("rsync"), "job output rides the ping");
}

/// A bare success carries no exit code and no duration, and those must read
/// back as absent rather than as zero — 0 is a real exit status.
#[tokio::test]
#[ignore]
async fn a_bare_success_stores_no_exit_code() {
    let Some(client) = common::ch_client_from_env().await else {
        eprintln!("CLICKHOUSE_URL unset; skipping");
        return;
    };
    let org_id = Uuid::new_v4();
    let sink =
        ClickhouseHeartbeatPingSink::new(client.clone(), uptimepage::storage::OrgTtlDays::new());
    sink.write_ping(&ping(org_id, Uuid::new_v4(), PingSignal::Success))
        .await;

    let rows = fetch(&client, org_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].signal, PingSignal::Success.as_enum8());
    assert_eq!(rows[0].exit_code, None);
    assert_eq!(rows[0].duration_ms, None);
}

/// The read matches the exact instant Postgres holds, not "the newest failure".
/// A ping whose log write was lost must show no output rather than the previous
/// run's, which would read as a diagnosis of the wrong failure.
#[tokio::test]
#[ignore]
async fn failure_output_is_matched_to_its_own_failure() {
    let Some(client) = common::ch_client_from_env().await else {
        eprintln!("CLICKHOUSE_URL unset; skipping");
        return;
    };
    let org = uptimepage::domain::OrgId(Uuid::new_v4());
    let target = Uuid::new_v4();
    let sink =
        ClickhouseHeartbeatPingSink::new(client.clone(), uptimepage::storage::OrgTtlDays::new());
    let store = uptimepage::storage::ClickhouseResultsStore::from_client(client);

    let older = chrono::Utc::now() - chrono::Duration::hours(2);
    sink.write_ping(&HeartbeatPingRecord {
        received_at: older,
        exit_code: Some(1),
        body: "disk full".into(),
        ..ping(org.0, target, PingSignal::Fail)
    })
    .await;

    assert_eq!(
        store
            .heartbeat_failure_output(org, target, older)
            .await
            .unwrap()
            .as_deref(),
        Some("disk full")
    );

    // A later failure whose body never landed reads as absent, not as "disk full".
    let newer = chrono::Utc::now();
    assert_eq!(
        store
            .heartbeat_failure_output(org, target, newer)
            .await
            .unwrap(),
        None
    );

    // Another tenant asking for the same target gets nothing.
    let stranger = uptimepage::domain::OrgId(Uuid::new_v4());
    assert_eq!(
        store
            .heartbeat_failure_output(stranger, target, older)
            .await
            .unwrap(),
        None
    );
}
