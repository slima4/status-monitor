//! Shared advisory-locked ticker for the periodic purge jobs.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::storage::locks::try_job;

/// Runs `purge` every `every` under the `job` advisory lock until `shutdown`
/// fires. The immediate first tick is skipped so boot isn't slowed by a
/// sweep; missed ticks are dropped — every purge is idempotent SQL, so
/// cadence drift is harmless. `job` keys the lock and the log lines.
pub async fn run_purge_loop<E: std::fmt::Display>(
    pool: PgPool,
    shutdown: CancellationToken,
    every: Duration,
    job: &'static str,
    purge: impl AsyncFn(&PgPool) -> Result<u64, E>,
) {
    let mut ticker = interval(every);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                try_job(&pool, job, || async {
                    match purge(&pool).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(deleted = n, "{job} purge"),
                        Err(err) => tracing::warn!(error = %err, "{job} purge failed"),
                    }
                })
                .await;
            }
        }
    }
}
