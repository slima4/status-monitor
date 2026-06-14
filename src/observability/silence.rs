//! Per-monitor silence (no-data) detection. A control-plane task that, every
//! tick, derives the set of *unmonitored* monitors — enabled targets whose every
//! assigned region has lost its probe (no fresh agent) — and records the
//! open/resolved boundary in `monitor_silence_state`, so a silence is announced
//! once and a recovery once.
//!
//! Slice 1 (this module): detect + record state + emit an operator gauge/log.
//! Customer notification rides the state transitions in a later slice.
//!
//! Sibling to `agent_health`: that one alarms a dead *agent* (operator-facing);
//! this rolls agent liveness up to the *monitor* so the customer whose monitor
//! is now blind can be told.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::observability::metrics::names;
use crate::storage::SilenceStore;

const TICK: Duration = Duration::from_secs(30);
/// Above this fraction of all enabled monitors silent at once, treat it as one
/// infra/probe outage (a region died), not thousands of independent silences:
/// state is still recorded, but a later slice damps customer delivery so our own
/// outage never storms every customer.
const MAX_SILENCE_FRACTION: f64 = 0.25;

pub async fn run(store: Arc<dyn SilenceStore>, stale_after: Duration, shutdown: CancellationToken) {
    let mut ticker = interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                if let Err(err) = sweep(store.as_ref(), stale_after).await {
                    tracing::warn!(error = %err, "silence sweep failed");
                }
            }
        }
    }
}

/// One sweep: diff the live unmonitored set against recorded-open silences,
/// entering the newly silent and resolving the recovered. Returns nothing —
/// errors are logged by `run`. Visible for tests.
pub async fn sweep(store: &dyn SilenceStore, stale_after: Duration) -> Result<()> {
    let now = Utc::now();
    // Maintenance-window suppression is a Slice-2 concern (it gates customer
    // delivery): during maintenance the agent stays alive, so a maintained
    // monitor doesn't enter the unmonitored set here anyway.
    let unmonitored: HashSet<_> = store
        .unmonitored(stale_after.as_secs())
        .await?
        .into_iter()
        .collect();
    let open: HashSet<_> = store.list_open().await?.into_iter().collect();

    for &(org, target_id) in unmonitored.difference(&open) {
        store.enter(org, target_id, now).await?;
    }
    for &(org, target_id) in open.difference(&unmonitored) {
        store.resolve(org, target_id, now).await?;
    }

    metrics::gauge!(names::MONITORS_UNMONITORED).set(unmonitored.len() as f64);

    if !unmonitored.is_empty() {
        let total = store.enabled_target_count().await?.max(0);
        let mass = total > 0 && (unmonitored.len() as f64) > MAX_SILENCE_FRACTION * total as f64;
        if mass {
            tracing::warn!(
                unmonitored = unmonitored.len(),
                total,
                "many monitors unmonitored at once — likely a regional probe outage, not per-monitor"
            );
        } else {
            tracing::warn!(
                unmonitored = unmonitored.len(),
                "monitors have no live probe (silent)"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::OrgId;
    use crate::storage::InMemorySilenceStore;
    use uuid::Uuid;

    #[tokio::test]
    async fn enters_new_and_resolves_recovered() {
        let org = OrgId(Uuid::now_v7());
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let store = InMemorySilenceStore::new();

        // First sweep: a and b are unmonitored → both entered.
        store.set_unmonitored(vec![(org, a), (org, b)]);
        sweep(&store, Duration::from_secs(120)).await.unwrap();
        assert_eq!(store.open_target_ids(), sorted(vec![a, b]));
        assert!(store.resolved_target_ids().is_empty());

        // Second sweep: a recovered (only b still silent) → a resolved, b stays.
        store.set_unmonitored(vec![(org, b)]);
        sweep(&store, Duration::from_secs(120)).await.unwrap();
        assert_eq!(store.open_target_ids(), vec![b]);
        assert_eq!(store.resolved_target_ids(), vec![a]);
    }

    #[tokio::test]
    async fn re_sweep_with_same_set_is_noop() {
        let org = OrgId(Uuid::now_v7());
        let a = Uuid::now_v7();
        let store = InMemorySilenceStore::new();
        store.set_unmonitored(vec![(org, a)]);
        for _ in 0..3 {
            sweep(&store, Duration::from_secs(120)).await.unwrap();
        }
        assert_eq!(store.open_target_ids(), vec![a]);
        assert!(store.resolved_target_ids().is_empty());
    }

    fn sorted(mut v: Vec<Uuid>) -> Vec<Uuid> {
        v.sort();
        v
    }
}
