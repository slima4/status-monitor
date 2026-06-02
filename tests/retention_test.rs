//! Live-PG (+CH for the no-op org cascade) tests for the daily retention
//! job, plus a pure test that the configured windows equal what the Privacy
//! Policy and the ClickHouse migration promise.
//! DB tests skipped by default; run under `--run-ignored` with `DATABASE_URL`
//! and `CLICKHOUSE_URL` set. Each test seeds rows tagged with a unique marker
//! and asserts only on its own rows against the shared dev DB.

mod common;

use common::make_user;
use uptimepage::config::{PublicStatusConfig, RetentionConfig, SessionConfig, TenancyConfig};
use uptimepage::jobs::retention::purge_old_data;
use uptimepage::public_status::PageCache;
use uptimepage::storage::create_org_with_owner;
use uuid::Uuid;

fn cache() -> PageCache {
    PageCache::new(&PublicStatusConfig::default())
}

async fn scalar_i64(pool: &sqlx::PgPool, sql: &str, marker: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(sql)
        .bind(marker)
        .fetch_one(pool)
        .await
        .expect("count");
    n
}

#[tokio::test]
#[ignore]
async fn purges_past_window_and_keeps_fresh_rows() {
    let Some(pool) = common::pg_pool_from_env().await else {
        return;
    };
    let Some(ch) = common::ch_client_from_env().await else {
        return;
    };

    let marker = format!("ret-{}", Uuid::new_v4().simple());
    let user = make_user(&pool, "retention").await;
    let slug = format!("retn-{}", &Uuid::new_v4().simple().to_string()[..6]);
    let org = create_org_with_owner(&pool, user, &slug, "Retention Test", 100)
        .await
        .expect("create org")
        .expect("org created")
        .id;

    // login_attempts: one well past 180d, one fresh.
    for (days_ago, _label) in [(200_i64, "old"), (1_i64, "fresh")] {
        sqlx::query(
            "INSERT INTO login_attempts (user_id, method, success, ip_hash, occurred_at) \
             VALUES ($1, 'test', false, $2, now() - ($3::int * INTERVAL '1 day'))",
        )
        .bind(user.0)
        .bind(&marker)
        .bind(days_ago)
        .execute(&pool)
        .await
        .expect("insert login_attempt");
    }
    // quota_events: one past 90d, one fresh.
    for days_ago in [120_i64, 1] {
        sqlx::query(
            "INSERT INTO quota_events (org_id, user_id, event, ip_hash, occurred_at) \
             VALUES ($1, $2, 'test', $3, now() - ($4::int * INTERVAL '1 day'))",
        )
        .bind(org.0)
        .bind(user.0)
        .bind(&marker)
        .bind(days_ago)
        .execute(&pool)
        .await
        .expect("insert quota_event");
    }
    // org_audit_log: one past 730d, one fresh. Tag the action with the marker.
    for days_ago in [800_i64, 1] {
        sqlx::query(
            "INSERT INTO org_audit_log (org_id, action, occurred_at) \
             VALUES ($1, $2, now() - ($3::int * INTERVAL '1 day'))",
        )
        .bind(org.0)
        .bind(&marker)
        .bind(days_ago)
        .execute(&pool)
        .await
        .expect("insert audit row");
    }
    // sessions: absolute-expired, idle-expired (alive absolute), and fresh.
    let sid = |k: &str| format!("{marker}-{k}");
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at, last_used_at) \
         VALUES ($1, $2, now() - INTERVAL '1 day', now())",
    )
    .bind(sid("expired"))
    .bind(user.0)
    .execute(&pool)
    .await
    .expect("insert expired session");
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at, last_used_at) \
         VALUES ($1, $2, now() + INTERVAL '30 days', now() - INTERVAL '60 days')",
    )
    .bind(sid("idle"))
    .bind(user.0)
    .execute(&pool)
    .await
    .expect("insert idle session");
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at, last_used_at) \
         VALUES ($1, $2, now() + INTERVAL '30 days', now())",
    )
    .bind(sid("fresh"))
    .bind(user.0)
    .execute(&pool)
    .await
    .expect("insert fresh session");

    let retention = RetentionConfig::default(); // 30/180/90/730
    let session = SessionConfig::default(); // idle 30d
    let grace = TenancyConfig::default().deletion_grace_period_days;

    purge_old_data(&pool, &ch, &retention, &session, grace, &cache())
        .await
        .expect("retention run");

    // Old rows gone, fresh rows survive — scoped to this run's marker.
    assert_eq!(
        scalar_i64(
            &pool,
            "SELECT count(*) FROM login_attempts WHERE ip_hash = $1",
            &marker
        )
        .await,
        1,
        "only the fresh login_attempt should remain"
    );
    assert_eq!(
        scalar_i64(
            &pool,
            "SELECT count(*) FROM quota_events WHERE ip_hash = $1",
            &marker
        )
        .await,
        1,
        "only the fresh quota_event should remain"
    );
    assert_eq!(
        scalar_i64(
            &pool,
            "SELECT count(*) FROM org_audit_log WHERE action = $1",
            &marker
        )
        .await,
        1,
        "only the fresh audit row should remain"
    );
    assert_eq!(
        scalar_i64(
            &pool,
            "SELECT count(*) FROM sessions WHERE id_hash LIKE $1 || '%'",
            &marker
        )
        .await,
        1,
        "only the fresh session should remain (absolute + idle both reaped)"
    );

    // Cleanup our own rows.
    let _ = sqlx::query("DELETE FROM login_attempts WHERE ip_hash = $1")
        .bind(&marker)
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM sessions WHERE id_hash LIKE $1 || '%'")
        .bind(&marker)
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org.0)
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.0)
        .execute(&pool)
        .await;
}

/// Pure: the code windows, the Privacy Policy table and the ClickHouse TTL
/// must agree. If a window changes without the policy/migration being
/// updated, this fails — one source of truth.
#[test]
fn windows_match_privacy_policy_and_clickhouse_ttl() {
    // check-result retention is the ClickHouse table TTLs, not an app-side
    // knob — keep these in lockstep with the literals in the CH migrations and
    // the public claims in `docs/legal/privacy.md`. Raw + 1m rollup live 90
    // days; the 1h rollup carries the aggregated history 13 months.
    const CHECK_RESULTS_RAW_DAYS: u32 = 90;
    const CHECK_RESULTS_HISTORY_MONTHS: u32 = 13;
    let r = RetentionConfig::default();
    let s = SessionConfig::default();
    let grace = TenancyConfig::default().deletion_grace_period_days;

    let policy = include_str!("../docs/legal/privacy.md");
    let want = [
        format!("| Check results (raw per-check detail) | {CHECK_RESULTS_RAW_DAYS} days"),
        format!(
            "| Check result history (aggregated, hourly) | {CHECK_RESULTS_HISTORY_MONTHS} months"
        ),
        format!("| Login attempts | {} days", r.login_attempts_days),
        format!("| Quota events | {} days", r.quota_events_days),
        format!("| Sessions | {} days maximum", s.absolute_timeout_days),
        format!("recoverable for {grace} days"),
    ];
    for w in &want {
        assert!(
            policy.contains(w.as_str()),
            "Privacy Policy is missing/!= the configured retention: expected to find {w:?}"
        );
    }
    // 730 days is written as "2 years" in the policy — assert both the prose
    // and the exact number so neither can drift silently.
    assert_eq!(r.audit_log_days, 730, "audit window changed");
    assert!(
        policy.contains("| Audit log | 2 years |"),
        "Privacy Policy audit-log retention line changed"
    );

    let m1 = include_str!("../migrations/clickhouse/001_initial.sql");
    assert!(
        m1.contains(&format!("INTERVAL {CHECK_RESULTS_RAW_DAYS} DAY")),
        "check_results raw ClickHouse TTL must equal CHECK_RESULTS_RAW_DAYS"
    );
    let m2 = include_str!("../migrations/clickhouse/002_check_results_1h.sql");
    assert!(
        m2.contains(&format!("INTERVAL {CHECK_RESULTS_HISTORY_MONTHS} MONTH")),
        "check_results_1h ClickHouse TTL must equal CHECK_RESULTS_HISTORY_MONTHS"
    );
}
