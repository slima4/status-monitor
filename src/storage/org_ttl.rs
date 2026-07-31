use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::Result;
use crate::quotas::service::{RetentionDays, retention_days_by_org};

/// Stamped on rows whose org isn't in the snapshot yet (just-created org, or a
/// boot before the first load). Matches the column DEFAULTs, so an unknown org
/// never over-retains.
const DEFAULT: RetentionDays = RetentionDays {
    row: 30,
    evidence: 7,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Resolves an org's physical retention windows for the write path. Bulk-loaded
/// from `plans` so a flush reads a lock, never the DB; the 64-entry
/// request-path plan cache would thrash under per-flush, all-org reads.
#[derive(Clone, Default)]
pub struct OrgTtlDays {
    snapshot: Arc<RwLock<HashMap<Uuid, RetentionDays>>>,
}

impl OrgTtlDays {
    pub fn new() -> Self {
        Self::default()
    }

    /// Windows for each org under a single read lock — one acquisition per
    /// insert batch, not per row. Unknown orgs get [`DEFAULT`].
    pub fn days_for_each(&self, org_ids: impl IntoIterator<Item = Uuid>) -> Vec<RetentionDays> {
        let snap = self.snapshot.read().expect("org ttl snapshot poisoned");
        org_ids
            .into_iter()
            .map(|id| snap.get(&id).copied().unwrap_or(DEFAULT))
            .collect()
    }

    /// Replace the snapshot from the shared retention reader, so physical TTL
    /// and the read-side window resolve the plan through the same path.
    pub async fn refresh(&self, pool: &PgPool) -> Result<usize> {
        let next = retention_days_by_org(pool).await?;
        let n = next.len();
        *self.snapshot.write().expect("org ttl snapshot poisoned") = next;
        Ok(n)
    }
}

/// Refreshes `ttl` every [`REFRESH_INTERVAL`] until cancelled. The first tick
/// fires immediately, so the snapshot warms right after spawn — no blocking
/// load on the boot path. A failed refresh keeps serving the last snapshot.
pub fn spawn_refresh(ttl: OrgTtlDays, pool: PgPool, shutdown: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => {
                    if let Err(err) = ttl.refresh(&pool).await {
                        tracing::warn!(?err, "org ttl refresh failed; serving last snapshot");
                    }
                }
            }
        }
    })
}
