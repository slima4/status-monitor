//! Daily purge of soft-deleted orgs and users past their grace period.
//!
//! Each tick runs the org cascade/outbox below, then a back-pressured
//! hard-delete of soft-deleted users whose grace window has elapsed and who
//! hold no live (unexpired, unused) recovery token. The user row's FK
//! `ON DELETE CASCADE` erases memberships, oauth_identities, api_tokens,
//! invitations, sessions and recovery tokens; rows that reference the user
//! as an actor (`login_attempts`, `org_audit_log`, `quota_events`,
//! `plan_overrides`) keep their rows with the actor nulled — audit
//! survives, identity does not.
//!
//! Two-step outbox pattern across Postgres and ClickHouse. The naive shape
//! ("DELETE in PG, then DELETE in CH") leaves orphan rows in ClickHouse if the
//! worker dies between the two — invisible to queries (the org no longer
//! exists in PG so no API path filters them), but they sit on disk forever and
//! break the "data fully erased within 30 days" privacy claim.
//!
//! Resolution: `clickhouse_purge_queue` is a durable handoff. The PG side
//! enqueues and cascades in one transaction; the CH side drains the queue
//! idempotently. A retry can replay either half without producing duplicates
//! or losing the request.
//!
//! Each tick:
//!  1. Selects orgs with `deleted_at` past the grace window (cap of 10 per
//!     tick — back-pressure against accidental mass deletion or a stuck CH).
//!  2. For each, runs a single PG transaction: insert into the queue (idem),
//!     then `DELETE FROM organizations` which cascades to every tenant table.
//!  3. Drains pending queue rows by issuing `ALTER ... DELETE` against
//!     ClickHouse. ClickHouse mutations are async server-side; we mark the
//!     queue row complete once the server accepts the mutation.
//!
//! Idempotency:
//!  * `ON CONFLICT (org_id) DO NOTHING` on the queue insert.
//!  * `ALTER TABLE ... DELETE WHERE org_id = ?` is safe to repeat — a second
//!    call on already-deleted rows is a no-op mutation.

use std::time::Duration;

use anyhow::Context;
use clickhouse::Client as ChClient;
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::OrgId;
use crate::error::Result;
use crate::public_status::PageCache;

/// Hard cap on orgs processed per tick. Small enough that a stuck ClickHouse
/// can't accumulate a huge backlog between alerts; large enough that a normal
/// week of churn drains in one or two ticks.
const PURGE_BATCH_LIMIT: i64 = 10;

/// Drain a chunk this big each tick. ClickHouse mutations are cheap to enqueue
/// but the server queues them serially, so going wider than this just trades
/// PG round-trips for CH mutation backlog.
const DRAIN_BATCH_LIMIT: i64 = 50;

/// Background loop: tick the purge job on `interval`, exit on cancellation.
/// First tick fires after one interval — startup time is when the rest of the
/// app is warming caches and we don't want to compete for connection slots.
/// `Skip` policy on the ticker prevents burst fires if the previous tick ran
/// long.
pub async fn run(
    pool: PgPool,
    ch: ChClient,
    interval: Duration,
    grace_days: u32,
    cache: PageCache,
    shutdown: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // Skip the immediate-fire that `interval` does on first poll.
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("purge worker shutting down");
                return;
            }
            _ = ticker.tick() => {
                match purge_tick(&pool, &ch, grace_days, &cache).await {
                    Ok(stats) => {
                        metrics::counter!("purge_deleted_total", "kind" => "org")
                            .increment(u64::from(stats.cascaded));
                        metrics::counter!("purge_deleted_total", "kind" => "user")
                            .increment(u64::from(stats.users));
                        tracing::info!(
                            orgs_purged = stats.cascaded,
                            users_purged = stats.users,
                            drained = stats.drained,
                            "purge tick complete"
                        );
                    }
                    Err(err) => tracing::error!(?err, "purge tick failed"),
                }
            }
        }
    }
}

/// Run one full purge cycle: cascade PG-side deletes for past-grace orgs,
/// then drain whatever pending CH purges exist (including ones enqueued on
/// previous ticks that didn't succeed).
pub async fn purge_tick(
    pool: &PgPool,
    ch: &ChClient,
    grace_days: u32,
    cache: &PageCache,
) -> Result<PurgeStats> {
    let cascaded = cascade_past_grace(pool, grace_days, cache).await?;
    let drained = drain_clickhouse_purge_queue(pool, ch).await?;
    // Users last: an org the user solo-owns may be cascaded above in the
    // same tick, so the user purge runs against already-settled org state.
    let users = purge_users_past_grace(pool, grace_days).await?;
    Ok(PurgeStats {
        cascaded,
        drained,
        users,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeStats {
    pub cascaded: u32,
    pub drained: u32,
    pub users: u32,
}

/// PG-side step: pick orgs past the grace window, enqueue + cascade in one
/// transaction per org. Returns the count actually cascaded so the caller can
/// emit a metric.
async fn cascade_past_grace(pool: &PgPool, grace_days: u32, cache: &PageCache) -> Result<u32> {
    let orgs: Vec<(Uuid,)> = sqlx::query_as(
        r#"SELECT id FROM organizations
           WHERE deleted_at IS NOT NULL
             AND deleted_at < now() - ($1::int * INTERVAL '1 day')
           ORDER BY deleted_at ASC
           LIMIT $2"#,
    )
    .bind(i64::from(grace_days))
    .bind(PURGE_BATCH_LIMIT)
    .fetch_all(pool)
    .await
    .context("purge: select past-grace orgs")?;

    let mut cascaded = 0u32;
    for (org_id,) in orgs {
        let mut tx = pool.begin().await.context("purge: begin tx")?;
        sqlx::query(
            r#"INSERT INTO clickhouse_purge_queue (org_id)
               VALUES ($1)
               ON CONFLICT (org_id) DO NOTHING"#,
        )
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .context("purge: enqueue ch")?;

        // ON DELETE CASCADE on every tenant table empties PG-side data.
        sqlx::query("DELETE FROM organizations WHERE id = $1")
            .bind(org_id)
            .execute(&mut *tx)
            .await
            .context("purge: cascade delete")?;

        tx.commit().await.context("purge: commit cascade")?;
        // Data is gone from Postgres now — drop any hot/last-good page so the
        // public surface can't keep serving a snapshot of a purged org.
        cache.invalidate(OrgId(org_id)).await;
        cascaded += 1;
        tracing::info!(%org_id, "org cascade-purged from postgres");
    }
    Ok(cascaded)
}

/// CH-side step: drain pending queue rows. Each row's two tables are deleted
/// independently; if either fails, the queue row's `attempts` and `last_error`
/// get incremented so the next tick retries. Successful drains record
/// `completed_at`.
pub async fn drain_clickhouse_purge_queue(pool: &PgPool, ch: &ChClient) -> Result<u32> {
    let pending: Vec<(Uuid,)> = sqlx::query_as(
        r#"SELECT org_id FROM clickhouse_purge_queue
           WHERE completed_at IS NULL
           ORDER BY queued_at ASC
           LIMIT $1"#,
    )
    .bind(DRAIN_BATCH_LIMIT)
    .fetch_all(pool)
    .await
    .context("purge: select pending queue")?;

    let mut drained = 0u32;
    for (org_id,) in pending {
        // Distinct tables, no shared server-side lock; run them in parallel
        // and use `join!` (not `try_join!`) so we still capture both errors.
        let (r1, r2) = tokio::join!(
            ch.query("ALTER TABLE check_results DELETE WHERE org_id = ?")
                .bind(org_id)
                .execute(),
            ch.query("ALTER TABLE check_results_1m DELETE WHERE org_id = ?")
                .bind(org_id)
                .execute(),
        );
        match (r1, r2) {
            (Ok(_), Ok(_)) => {
                sqlx::query(
                    r#"UPDATE clickhouse_purge_queue
                       SET completed_at = now()
                       WHERE org_id = $1"#,
                )
                .bind(org_id)
                .execute(pool)
                .await
                .context("purge: mark queue complete")?;
                drained += 1;
                tracing::info!(%org_id, "clickhouse purge complete");
            }
            (e1, e2) => {
                let err = format!("{:?} | {:?}", e1.err(), e2.err());
                sqlx::query(
                    r#"UPDATE clickhouse_purge_queue
                       SET attempts = attempts + 1, last_error = $2
                       WHERE org_id = $1"#,
                )
                .bind(org_id)
                .bind(&err)
                .execute(pool)
                .await
                .context("purge: mark queue attempt")?;
                tracing::warn!(%org_id, %err, "clickhouse purge failed; will retry");
            }
        }
    }
    Ok(drained)
}

/// Hard-delete soft-deleted users whose grace window has elapsed, skipping
/// anyone who still holds a live (unexpired, unused) recovery token — the
/// recovery window is enforced by this predicate, not by re-deriving the
/// grace period in the recovery endpoint. Postgres has no `DELETE … LIMIT`,
/// so the batch bound is a subquery; the same `PURGE_BATCH_LIMIT`
/// back-pressure as the org cascade caps a runaway mass-deletion. The
/// `users` FK cascade erases dependent rows; `login_attempts` /
/// `org_audit_log` keep theirs with the actor nulled.
pub async fn purge_users_past_grace(pool: &PgPool, grace_days: u32) -> Result<u32> {
    let purged: Vec<(Uuid,)> = sqlx::query_as(
        r#"DELETE FROM users
           WHERE id IN (
               SELECT id FROM users
                WHERE deleted_at IS NOT NULL
                  AND deleted_at < now() - ($1::int * INTERVAL '1 day')
                  AND NOT EXISTS (
                      SELECT 1 FROM user_recovery_tokens t
                       WHERE t.user_id = users.id
                         AND t.expires_at > now()
                         AND t.used_at IS NULL
                  )
                ORDER BY deleted_at ASC
                LIMIT $2
           )
           RETURNING id"#,
    )
    .bind(i64::from(grace_days))
    .bind(PURGE_BATCH_LIMIT)
    .fetch_all(pool)
    .await
    .context("purge: delete past-grace users")?;

    for (id,) in &purged {
        tracing::debug!(%id, "user hard-purged");
    }
    Ok(u32::try_from(purged.len()).unwrap_or(u32::MAX))
}
