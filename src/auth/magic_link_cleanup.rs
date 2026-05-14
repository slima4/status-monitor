//! Periodic magic-link cleanup: drops expired rows and used rows older than
//! the forensic window. Idempotent SQL; cadence drift is harmless.
//!
//! Spawned only when `magic_link` is in `auth.enabled_methods`; otherwise the
//! table only ever holds anti-enumeration residue that the next sweep clears.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::auth::magic_link;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub async fn run(pool: PgPool, shutdown: CancellationToken) {
    let mut ticker = interval(CLEANUP_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await; // skip the immediate first tick
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => match magic_link::purge_old(&pool).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(deleted = n, "magic_link purge"),
                Err(err) => tracing::warn!(error = %err, "magic_link purge failed"),
            },
        }
    }
}
