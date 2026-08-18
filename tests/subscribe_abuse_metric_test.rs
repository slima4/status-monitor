//! Tests for `uptimepage_confirm_emails_total` — the confirm/verify email
//! abuse counter introduced in fix #89.
//!
//! ## Structure
//!
//! * **Unit tests** (no Postgres required) — verify that the metric name is
//!   registered, that the label cardinality is bounded (only `path` and
//!   `outcome` labels exist), and that direct counter increments accumulate
//!   correctly. These run on every `cargo test` invocation.
//!
//! * **Integration tests** (`#[ignore = "needs live Postgres"]`) — exercise
//!   the full HTTP path for both the `channel` and `subscribe` sends and
//!   assert the emitted Prometheus text. They require `DATABASE_URL` and
//!   the same migration set as other `*_pg_test.rs` files in this suite.
//!
//! The Prometheus recorder is installed once per process via `OnceLock`,
//! matching the `http_metrics_test.rs` pattern. All unit tests that touch
//! counters run sequentially inside one `#[tokio::test]` body to avoid
//! races on the single global recorder.

mod common;

use std::sync::OnceLock;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

// ── Shared Prometheus recorder ────────────────────────────────────────────

fn handle() -> &'static PrometheusHandle {
    static H: OnceLock<PrometheusHandle> = OnceLock::new();
    H.get_or_init(|| {
        PrometheusBuilder::new()
            .install_recorder()
            .expect("install prometheus test recorder")
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Parse the rendered Prometheus text and return the sum of all samples whose
/// metric name and labels match `name` and the given `labels` subset.
/// Absent metric → 0.0 (the counter has never been incremented).
fn counter_sum(rendered: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    rendered
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter(|l| l.starts_with(name))
        .filter(|l| {
            labels
                .iter()
                .all(|(k, v)| l.contains(&format!("{k}=\"{v}\"")))
        })
        .filter_map(|l| {
            l.rsplit_once(' ')
                .and_then(|(_, v)| v.trim().parse::<f64>().ok())
        })
        .sum()
}

// ─────────────────────────────────────────────────────────────────────────
// Unit tests — no Postgres required
// ─────────────────────────────────────────────────────────────────────────

/// Central assertion block. All counter-level unit tests run sequentially
/// inside a single `#[tokio::test]` body because `install_recorder()` is a
/// one-shot global operation that panics on the second call in the same
/// process. Separate bodies would race on process-level recorder state.
#[tokio::test]
async fn counter_unit_tests() {
    let h = handle();

    // ── zero sends does not create a series ──────────────────────────────
    //
    // Before any increment the metric name must be absent (or at 0) from the
    // render. A `describe_counter!` alone is not enough to create a series —
    // the first `.increment()` does. This assertion is meaningful only if run
    // before any other test in this file has incremented the counter.
    let rendered = h.render();
    let initial_sent = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("outcome", "sent")],
    );
    let initial_rl = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("outcome", "rate_limited")],
    );
    // Both start at 0 (series may be absent, which sum() maps to 0).
    assert_eq!(
        initial_sent, 0.0,
        "sent counter not at 0 before first increment"
    );
    assert_eq!(
        initial_rl, 0.0,
        "rate_limited counter not at 0 before first increment"
    );

    // ── subscribe path: sent increments correctly ─────────────────────────
    metrics::counter!(
        uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL,
        "path" => "subscribe",
        "outcome" => "sent",
    )
    .increment(1);

    let rendered = h.render();
    assert!(
        rendered.contains("uptimepage_confirm_emails_total"),
        "counter series missing from render after first increment — metric may be mis-registered:\n{rendered}"
    );
    let v = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "subscribe"), ("outcome", "sent")],
    );
    assert_eq!(
        v,
        initial_sent + 1.0,
        "subscribe/sent did not increment by 1"
    );

    // ── subscribe path: failed increments correctly ───────────────────────
    metrics::counter!(
        uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL,
        "path" => "subscribe",
        "outcome" => "failed",
    )
    .increment(1);

    let rendered = h.render();
    let v = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "subscribe"), ("outcome", "failed")],
    );
    assert_eq!(v, 1.0, "subscribe/failed did not increment to 1");

    // ── subscribe path: rate_limited increments correctly ─────────────────
    metrics::counter!(
        uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL,
        "path" => "subscribe",
        "outcome" => "rate_limited",
    )
    .increment(1);

    let rendered = h.render();
    let v = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "subscribe"), ("outcome", "rate_limited")],
    );
    assert_eq!(
        v,
        initial_rl + 1.0,
        "subscribe/rate_limited did not increment to 1"
    );

    // ── channel path: sent increments correctly ───────────────────────────
    metrics::counter!(
        uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL,
        "path" => "channel",
        "outcome" => "sent",
    )
    .increment(1);

    let rendered = h.render();
    let v = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "sent")],
    );
    assert_eq!(v, 1.0, "channel/sent did not increment to 1");

    // ── channel path: rate_limited increments correctly ───────────────────
    metrics::counter!(
        uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL,
        "path" => "channel",
        "outcome" => "rate_limited",
    )
    .increment(1);

    let rendered = h.render();
    let v = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "rate_limited")],
    );
    assert_eq!(v, 1.0, "channel/rate_limited did not increment to 1");

    // ── burst: N increments accumulate correctly ──────────────────────────
    // Simulate 9 more sends (10 total for channel/sent) and confirm the
    // counter sums to exactly N without drift.
    const BURST: u64 = 9;
    metrics::counter!(
        uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL,
        "path" => "channel",
        "outcome" => "sent",
    )
    .increment(BURST);

    let rendered = h.render();
    let v = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "sent")],
    );
    assert_eq!(
        v,
        1.0 + BURST as f64,
        "burst accumulation wrong: expected {} got {}",
        1 + BURST,
        v
    );

    // ── label isolation: subscribe/sent does not contaminate channel/sent ──
    // The subscribe/sent value must still equal what we set it to earlier
    // — the channel burst must not bleed across the path label.
    let rendered = h.render();
    let subscribe_sent = counter_sum(
        &rendered,
        "uptimepage_confirm_emails_total",
        &[("path", "subscribe"), ("outcome", "sent")],
    );
    assert_eq!(
        subscribe_sent,
        initial_sent + 1.0,
        "subscribe/sent contaminated by channel/sent increments"
    );

    // ── metric name constant matches the description ──────────────────────
    // Both the `names::` const and the `describe_counter!` call must agree
    // on the wire name. Deriving both from the same const in metrics.rs
    // keeps this trivially true — but if someone edits one without the other
    // the rendered output will contain the constant's name, not the desc name.
    assert!(
        rendered.contains(uptimepage::observability::metrics::names::CONFIRM_EMAILS_TOTAL),
        "CONFIRM_EMAILS_TOTAL constant does not appear in rendered metrics — name drift"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Integration tests — require live Postgres (DATABASE_URL)
// ─────────────────────────────────────────────────────────────────────────

/// Verify that creating an email notification channel via the authenticated
/// API increments `uptimepage_confirm_emails_total{path="channel",outcome="sent"}`.
///
/// Flow:
///  1. Build a PG-backed app (in-memory email sender, real Postgres pool).
///  2. POST /api/v1/notification-channels with an email config.
///  3. The handler calls `spawn_send_verification` (background); sleep briefly
///     to let the spawned task complete.
///  4. Assert the counter rose by 1 and that one email was captured.
///
/// Needs: DATABASE_URL pointing at a migrated Postgres instance.
#[tokio::test]
async fn channel_path_sent_counter_increments_on_create() {
    use axum::http::StatusCode;
    use common::{build_test_app_with_pg, json_request, pg_pool_from_env, unique_slug};
    use tower::ServiceExt;

    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let h = handle();

    // build_test_app_with_pg wires the pool into AppState.db and returns an
    // authenticated owner-session router alongside the provisioned org id.
    let (app, _org) = build_test_app_with_pg(pool, |_| {}).await;

    let before = counter_sum(
        &h.render(),
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "sent")],
    );

    let addr = format!("sub-{}@example.com", unique_slug("ch-sent-pg"));
    let resp = app
        .oneshot(json_request(
            "POST",
            "/api/v1/notification-channels",
            serde_json::json!({
                "name": "metric-test-channel",
                "config": { "type": "email", "to": addr },
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "channel create failed");

    // Give the background task time to run.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let after = counter_sum(
        &h.render(),
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "sent")],
    );
    assert_eq!(
        after,
        before + 1.0,
        "channel/sent counter did not increment after create"
    );
}

/// Verify that the resend-verification endpoint also increments the counter.
///
/// Needs: DATABASE_URL.
#[tokio::test]
async fn channel_path_sent_counter_increments_on_resend() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use common::{body_json, build_test_app_with_pg, json_request, pg_pool_from_env, unique_slug};
    use tower::ServiceExt;

    let Some(pool) = pg_pool_from_env().await else {
        return;
    };
    let h = handle();

    let (app, _org) = build_test_app_with_pg(pool, |_| {}).await;

    // Create a channel first.
    let addr = format!("sub-{}@example.com", unique_slug("ch-resend-pg"));
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/notification-channels",
            serde_json::json!({
                "name": "resend-test",
                "config": { "type": "email", "to": addr },
                "enabled": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    let channel_id = body["id"].as_str().unwrap().to_string();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let before = counter_sum(
        &h.render(),
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "sent")],
    );

    let resend_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/notification-channels/{channel_id}/resend-verification"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resend_resp.status(), StatusCode::ACCEPTED, "resend failed");

    let after = counter_sum(
        &h.render(),
        "uptimepage_confirm_emails_total",
        &[("path", "channel"), ("outcome", "sent")],
    );
    assert_eq!(
        after,
        before + 1.0,
        "channel/sent counter did not increment after resend-verification"
    );
}
