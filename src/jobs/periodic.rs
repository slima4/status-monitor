//! Shared advisory-locked ticker for the periodic purge jobs.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;

use crate::storage::locks::try_job;

/// Runs `purge` every `every` under the `job` advisory lock until `shutdown`
/// fires, the first run a whole period away: nothing is expired at boot that
/// was not expired a moment before it. Missed ticks are dropped, since every
/// job here is idempotent.
pub async fn run_purge_loop<E: std::fmt::Display>(
    pool: PgPool,
    shutdown: CancellationToken,
    every: Duration,
    job: &'static str,
    purge: impl AsyncFn(&PgPool) -> Result<u64, E>,
) {
    run_loop_at(pool, shutdown, every, every, job, purge).await
}

/// The same loop, run once at startup before settling into `every`. A job that
/// converges state rather than deletes expired rows needs this: the state it
/// answers to can change while the process is down, and a host that redeploys
/// more often than the period would otherwise never reach a tick.
pub async fn run_purge_loop_from_boot<E: std::fmt::Display>(
    pool: PgPool,
    shutdown: CancellationToken,
    every: Duration,
    job: &'static str,
    purge: impl AsyncFn(&PgPool) -> Result<u64, E>,
) {
    run_loop_at(pool, shutdown, Duration::ZERO, every, job, purge).await
}

async fn run_loop_at<E: std::fmt::Display>(
    pool: PgPool,
    shutdown: CancellationToken,
    first: Duration,
    every: Duration,
    job: &'static str,
    purge: impl AsyncFn(&PgPool) -> Result<u64, E>,
) {
    let mut ticker = interval_at(Instant::now() + first, every);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                try_job(&pool, job, || async {
                    match purge(&pool).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(rows = n, "{job}"),
                        Err(err) => tracing::warn!(error = %err, "{job} failed"),
                    }
                })
                .await;
            }
        }
    }
}
