//! Periodic invitation cleanup: drops rows that have aged out of the UI's
//! "recent invitations" window. Pure DELETE — there is no business state left
//! to reconcile once an invitation has been accepted, declined or expired.
//!
//! Cadence: every 6h. Drift is fine — the SQL filter is idempotent and the
//! row count per pass is bounded by the per-org quota times the active-org
//! count.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::auth::invitations;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Background task body. Runs until `shutdown` fires.
///
/// `keep_history_days` controls how long *resolved* (accepted / declined) and
/// *expired* invitations stay in the table for the UI's history list. The
/// caller derives this from `auth.invitations.expiry_hours`.
pub async fn run(pool: PgPool, keep_history_days: i64, shutdown: CancellationToken) {
    let mut ticker = interval(CLEANUP_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // The first tick fires immediately; skip it so startup isn't slowed down
    // by a sweep we don't yet have data for.
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => match invitations::purge_old(&pool, keep_history_days).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(deleted = n, "invitation purge"),
                Err(err) => tracing::warn!(error = %err, "invitation purge failed"),
            },
        }
    }
}
