//! Periodic channel-verification-token cleanup: drops expired rows and used
//! rows older than the forensic window. Idempotent SQL; cadence drift is
//! harmless.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::storage::channel_verification;
use crate::storage::locks::try_job;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub async fn run(pool: PgPool, shutdown: CancellationToken) {
    let mut ticker = interval(CLEANUP_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await; // skip the immediate first tick
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                try_job(&pool, "channel_verification_cleanup", || async {
                    match channel_verification::purge_old(&pool).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(deleted = n, "channel verification purge"),
                        Err(err) => tracing::warn!(error = %err, "channel verification purge failed"),
                    }
                })
                .await;
            }
        }
    }
}
