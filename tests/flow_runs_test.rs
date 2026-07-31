//! Storage contract for browser-flow run telemetry: the trace round-trips as
//! parallel arrays, both retention windows are stamped from the org's plan, and
//! the page snapshot expires ahead of the run that carries it.

mod common;

use chrono::{Duration, Utc};
use clickhouse::Row;
use serde::Deserialize;
use uuid::Uuid;

use uptimepage::domain::CheckStatus;
use uptimepage::domain::agent_wire::{
    ConsoleLine, FlowEvidence, FlowRunRecord, StepOutcome, StepTrace,
};
use uptimepage::storage::ClickhouseFlowRunSink;
use uptimepage::storage::traits::FlowRunSink;

fn step(op: &str, outcome: StepOutcome, ms: u32) -> StepTrace {
    StepTrace {
        op: op.into(),
        outcome,
        duration_ms: ms,
    }
}

fn failed_login(org_id: Uuid, target_id: Uuid) -> FlowRunRecord {
    FlowRunRecord {
        org_id,
        target_id,
        timestamp: Utc::now(),
        status: CheckStatus::Down,
        duration_ms: 2570,
        error: Some("step 4/5 assert_url: url does not contain \"/secure\"".into()),
        steps: vec![
            step("fill", StepOutcome::Passed, 12),
            step("fill", StepOutcome::Passed, 9),
            step("click", StepOutcome::Passed, 41),
            step("assert_url", StepOutcome::Failed, 2000),
            step("assert_text", StepOutcome::Skipped, 0),
        ],
        evidence: Some(FlowEvidence {
            final_url: Some("https://app.example.com/login".into()),
            title: Some("Sign in".into()),
            text_snippet: Some("Your password is invalid!".into()),
            console: vec![ConsoleLine {
                level: "error".into(),
                text: "token expired".into(),
            }],
        }),
    }
}

#[derive(Debug, Row, Deserialize)]
struct StoredRun {
    status: i8,
    duration_ms: u32,
    stopped_step: Option<u16>,
    error: String,
    step_op: Vec<String>,
    step_outcome: Vec<i8>,
    step_ms: Vec<u32>,
    final_url: String,
    title: String,
    text_snippet: String,
    console_text: Vec<String>,
    evidence_days: u16,
    ttl_days: u16,
}

const SELECT: &str = "SELECT status, duration_ms, stopped_step, error, step_op, step_outcome, \
     step_ms, final_url, title, text_snippet, console_text, evidence_days, ttl_days \
     FROM flow_runs WHERE org_id = ? ORDER BY timestamp";

async fn fetch(client: &clickhouse::Client, org_id: Uuid) -> Vec<StoredRun> {
    client
        .query(SELECT)
        .bind(org_id)
        .fetch_all::<StoredRun>()
        .await
        .expect("read flow_runs")
}

#[tokio::test]
#[ignore]
async fn a_failed_run_stores_its_trace_and_page_alongside_the_verdict() {
    let Some(client) = common::ch_client_from_env().await else {
        eprintln!("CLICKHOUSE_URL unset; skipping");
        return;
    };
    let org_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let sink = ClickhouseFlowRunSink::new(
        client.clone(),
        "eu-helsinki".into(),
        uptimepage::storage::OrgTtlDays::new(),
    );
    sink.write_runs(&[failed_login(org_id, target_id)]).await;

    let rows = fetch(&client, org_id).await;
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.status, CheckStatus::Down.as_enum8());
    assert_eq!(r.duration_ms, 2570);
    assert_eq!(
        r.stopped_step,
        Some(3),
        "the step the run stopped on is derived from the trace, 0-based"
    );
    assert!(r.error.contains("assert_url"));

    assert_eq!(
        r.step_op,
        vec!["fill", "fill", "click", "assert_url", "assert_text"]
    );
    assert_eq!(
        r.step_outcome,
        vec![1, 1, 1, 2, 3],
        "passed / failed / skipped round-trip as the declared enum"
    );
    assert_eq!(r.step_ms, vec![12, 9, 41, 2000, 0]);

    assert_eq!(r.final_url, "https://app.example.com/login");
    assert_eq!(r.title, "Sign in");
    assert_eq!(r.text_snippet, "Your password is invalid!");
    assert_eq!(r.console_text, vec!["token expired"]);

    // An org the TTL snapshot has never seen falls back to the column defaults
    // rather than over-retaining.
    assert_eq!(r.ttl_days, 30);
    assert_eq!(r.evidence_days, 7);
}

#[tokio::test]
#[ignore]
async fn a_passing_run_stores_the_trace_and_no_page() {
    let Some(client) = common::ch_client_from_env().await else {
        eprintln!("CLICKHOUSE_URL unset; skipping");
        return;
    };
    let org_id = Uuid::new_v4();
    let sink = ClickhouseFlowRunSink::new(
        client.clone(),
        "eu-helsinki".into(),
        uptimepage::storage::OrgTtlDays::new(),
    );
    sink.write_runs(&[FlowRunRecord {
        org_id,
        target_id: Uuid::new_v4(),
        timestamp: Utc::now(),
        status: CheckStatus::Up,
        duration_ms: 1900,
        error: None,
        steps: vec![
            step("fill", StepOutcome::Passed, 12),
            step("assert_url", StepOutcome::Passed, 300),
        ],
        evidence: None,
    }])
    .await;

    let rows = fetch(&client, org_id).await;
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.stopped_step, None, "nothing failed, so nothing is blamed");
    assert_eq!(r.step_outcome, vec![1, 1]);
    assert!(r.final_url.is_empty());
    assert!(r.text_snippet.is_empty());
    assert!(r.console_text.is_empty());
    assert_eq!(
        r.step_ms,
        vec![12, 300],
        "durations are kept on a pass — that is what makes a step trend"
    );
}

// The whole point of the two windows: a run old enough to have shed its page
// snapshot still answers what ran and how long it took.
#[tokio::test]
#[ignore]
async fn page_evidence_expires_while_the_trace_survives() {
    let Some(client) = common::ch_client_from_env().await else {
        eprintln!("CLICKHOUSE_URL unset; skipping");
        return;
    };
    let org_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let sink = ClickhouseFlowRunSink::new(
        client.clone(),
        "eu-helsinki".into(),
        uptimepage::storage::OrgTtlDays::new(),
    );

    let mut stale = failed_login(org_id, target_id);
    stale.timestamp = Utc::now() - Duration::days(10);
    let fresh = failed_login(org_id, target_id);
    sink.write_runs(&[stale, fresh]).await;

    // TTLs are applied on merge, so force one rather than waiting for it.
    client
        .query("OPTIMIZE TABLE flow_runs FINAL")
        .execute()
        .await
        .expect("optimize flow_runs");

    let rows = fetch(&client, org_id).await;
    assert_eq!(rows.len(), 2, "the row itself lives its full ttl_days");
    let (old, new) = (&rows[0], &rows[1]);

    assert!(
        old.text_snippet.is_empty() && old.final_url.is_empty() && old.title.is_empty(),
        "page content past evidence_days must be gone: {old:?}"
    );
    assert!(old.console_text.is_empty());
    assert_eq!(
        old.step_op.len(),
        5,
        "the trace carries no page content, so it stays"
    );
    assert_eq!(old.step_outcome, vec![1, 1, 1, 2, 3]);
    assert_eq!(old.stopped_step, Some(3));

    assert_eq!(
        new.text_snippet, "Your password is invalid!",
        "a run inside the window keeps its page"
    );
}
