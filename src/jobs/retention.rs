//! Daily long-horizon data retention. Wakes once a day at 03:00 UTC.
//!
//! This job owns only the *long* windows. The short-cadence security sweeps
//! (`oauth_state_cleanup`, `magic_link_cleanup`) keep their own loops on
//! purpose: their *frequency* is the security property, so folding a 10-minute
//! window into a 24-hour tick would silently widen it.
//!
//! Each tick, in order:
//!  1. The soft-deleted org cascade + ClickHouse drain + recovery-aware user
//!     hard-purge — reused from [`purge_deleted`], not reimplemented.
//!  2. Row deletes for `login_attempts`, `quota_events`, `org_audit_log` past
//!     their configured windows. Every window is bound from `[retention]`
//!     config — never a literal — so config, code and the Privacy Policy
//!     cannot drift apart.
//!  3. The session sweep: a row is reaped when it passes its absolute expiry
//!     **or** its idle window. Idle (`idle_timeout_days`, 30d) is the binding
//!     constraint; absolute (90d) is the policy ceiling. So an abandoned
//!     session that is never looked up again still can't outlive the Cookie
//!     Policy's idle promise at rest.
//!
//! `check_results` (ClickHouse) is deliberately NOT mutated here. Its
//! retention is the table's own `TTL` (a background merge). A broad
//! `ALTER … DELETE` would queue serially ahead of the per-org GDPR-erasure
//! mutations and let erasure be reported "done" while bytes still sit on disk.

use std::time::Duration;

use anyhow::Context;
use chrono::{Timelike, Utc};
use clickhouse::Client as ChClient;
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::config::{RetentionConfig, SessionConfig};
use crate::error::Result;
use crate::jobs::purge_deleted::{self, PurgeStats, QueueDepth};
use crate::public_status::PageCache;

/// `check_results` retention is the ClickHouse table `TTL`, so this job does
/// no arithmetic on `check_results_days`. But the unlimited / self-host plan
/// paths can yield an `i32::MAX`-ish sentinel; at or beyond this we emit one
/// explicit "retention effectively disabled" log line rather than letting a
/// nonsense window pass silently.
const RETENTION_DISABLED_THRESHOLD_DAYS: u32 = 36_500; // ~100 years

const SECONDS_PER_DAY: u64 = 86_400;
const RUN_HOUR_UTC: u32 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub purge: PurgeStats,
    pub purge_queue: QueueDepth,
    pub login_attempts: u64,
    pub quota_events: u64,
    pub audit_log: u64,
    pub sessions: u64,
}

/// Whole seconds from now until the next 03:00 UTC.
fn secs_until_next_run() -> u64 {
    let now = Utc::now();
    let secs_today =
        u64::from(now.hour()) * 3600 + u64::from(now.minute()) * 60 + u64::from(now.second());
    let target = u64::from(RUN_HOUR_UTC) * 3600;
    if secs_today < target {
        target - secs_today
    } else {
        SECONDS_PER_DAY - secs_today + target
    }
}

/// Background loop: first fire at the next 03:00 UTC, then every 24h. `Skip`
/// missed-tick policy so a long tick can't burst-fire on the next poll.
pub async fn run(
    pool: PgPool,
    ch: ChClient,
    retention: RetentionConfig,
    session: SessionConfig,
    grace_days: u32,
    cache: PageCache,
    shutdown: CancellationToken,
) {
    tokio::select! {
        _ = shutdown.cancelled() => return,
        _ = tokio::time::sleep(Duration::from_secs(secs_until_next_run())) => {}
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(SECONDS_PER_DAY));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("retention worker shutting down");
                return;
            }
            _ = ticker.tick() => {
                match purge_old_data(&pool, &ch, &retention, &session, grace_days, &cache).await {
                    Ok(report) => {
                        emit_metrics(&report);
                        tracing::info!(?report, "retention tick complete");
                    }
                    Err(err) => tracing::error!(?err, "retention tick failed"),
                }
            }
        }
    }
}

fn emit_metrics(r: &RetentionReport) {
    metrics::counter!("purge_deleted_total", "kind" => "org")
        .increment(u64::from(r.purge.cascaded));
    metrics::counter!("purge_deleted_total", "kind" => "user").increment(u64::from(r.purge.users));
    metrics::counter!("retention_purged_rows_total", "table" => "login_attempts")
        .increment(r.login_attempts);
    metrics::counter!("retention_purged_rows_total", "table" => "quota_events")
        .increment(r.quota_events);
    metrics::counter!("retention_purged_rows_total", "table" => "org_audit_log")
        .increment(r.audit_log);
    metrics::counter!("retention_purged_rows_total", "table" => "sessions").increment(r.sessions);
    // Gauges, not counters: depth/age describe a *current* backlog. A
    // sustained non-zero pending count (or a climbing oldest-age) is the
    // alert condition for a stuck ClickHouse erasure path.
    metrics::gauge!("clickhouse_purge_queue_pending").set(r.purge_queue.pending as f64);
    metrics::gauge!("clickhouse_purge_queue_oldest_age_seconds")
        .set(r.purge_queue.oldest_age_secs as f64);
}

/// One full retention cycle. Public so tests drive it directly (small windows,
/// just-past rows, assert deletion) without waiting for a daily tick.
pub async fn purge_old_data(
    pool: &PgPool,
    ch: &ChClient,
    retention: &RetentionConfig,
    session: &SessionConfig,
    grace_days: u32,
    cache: &PageCache,
) -> Result<RetentionReport> {
    let purge = purge_deleted::purge_tick(pool, ch, grace_days, cache).await?;
    let purge_queue = purge_deleted::purge_queue_depth(pool).await?;

    let login_attempts = delete_older_than(
        pool,
        "DELETE FROM login_attempts \
         WHERE occurred_at < now() - ($1::int * INTERVAL '1 day')",
        retention.login_attempts_days,
        "retention: login_attempts",
    )
    .await?;
    let quota_events = delete_older_than(
        pool,
        "DELETE FROM quota_events \
         WHERE occurred_at < now() - ($1::int * INTERVAL '1 day')",
        retention.quota_events_days,
        "retention: quota_events",
    )
    .await?;
    let audit_log = delete_older_than(
        pool,
        "DELETE FROM org_audit_log \
         WHERE occurred_at < now() - ($1::int * INTERVAL '1 day')",
        retention.audit_log_days,
        "retention: org_audit_log",
    )
    .await?;

    let sessions = sweep_sessions(pool, session.idle_timeout_days).await?;

    if retention.check_results_days >= RETENTION_DISABLED_THRESHOLD_DAYS {
        tracing::warn!(
            days = retention.check_results_days,
            "check_results retention effectively disabled by config; relying on ClickHouse TTL only"
        );
    }

    Ok(RetentionReport {
        purge,
        purge_queue,
        login_attempts,
        quota_events,
        audit_log,
        sessions,
    })
}

/// Cross-tenant by design (this is the retention sweep) — `query` is a static
/// literal per call site, the only bound is the `$1` day window.
async fn delete_older_than(
    pool: &PgPool,
    query: &'static str,
    days: u32,
    ctx: &'static str,
) -> Result<u64> {
    let res = sqlx::query(query)
        .bind(i64::from(days))
        .execute(pool)
        .await
        .context(ctx)?;
    Ok(res.rows_affected())
}

/// Reap sessions on absolute expiry OR idle. `idle_days` comes from
/// `auth.session.idle_timeout_days` — the same number `session::lookup`
/// enforces in-band — so the Cookie Policy's idle promise holds at rest too.
async fn sweep_sessions(pool: &PgPool, idle_days: u32) -> Result<u64> {
    let res = sqlx::query(
        "DELETE FROM sessions \
         WHERE expires_at < now() \
            OR last_used_at < now() - ($1::int * INTERVAL '1 day')",
    )
    .bind(i64::from(idle_days))
    .execute(pool)
    .await
    .context("retention: sweep sessions")?;
    Ok(res.rows_affected())
}
