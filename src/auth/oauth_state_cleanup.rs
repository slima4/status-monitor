//! Periodic deletion of expired `oauth_states` rows.
//!
//! `oauth_states.expires_at` is 10 minutes from insert (AUTH §7.8). The
//! callback's atomic DELETE-and-RETURN purges any row it successfully consumes,
//! so a row only outlives its TTL when the user abandons the OAuth flow.
//! Without this task the table accumulates stale rows forever — a slow,
//! silent leak that the database catches up on at unlucky times. The runtime
//! cost is one `DELETE … WHERE expires_at < now()` per period.
//!
//! Schedule: every 10 minutes, with a missed-tick skip so a flapping system
//! doesn't queue up duplicate work.

use std::time::Duration;

use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

const PERIOD: Duration = Duration::from_secs(10 * 60);

/// Spawns the cleanup loop. Returns the join handle so `main` can drive
/// graceful shutdown alongside the rest of the background workers.
pub fn spawn(pool: PgPool, shutdown: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(PERIOD);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // First tick fires immediately — skip it so boot doesn't race the
        // app's other warm-up work.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => {
                    match purge_expired(&pool).await {
                        Ok(n) if n > 0 => {
                            tracing::debug!(removed = n, "oauth_state_cleanup: purged expired states");
                        }
                        Ok(_) => {}
                        Err(err) => {
                            tracing::warn!(error = %err, "oauth_state_cleanup: purge failed");
                        }
                    }
                }
            }
        }
    })
}

pub async fn purge_expired(pool: &PgPool) -> sqlx::Result<u64> {
    let res = sqlx::query("DELETE FROM oauth_states WHERE expires_at < now()")
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
