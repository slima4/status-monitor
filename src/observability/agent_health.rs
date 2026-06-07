//! Dead-man's-switch for regional agents. A control-plane task that periodically
//! reads `agents.last_seen_at` and exposes per-agent staleness as Prometheus
//! gauges (alertable in Grafana) plus a warn log on the fresh→stale transition.
//! Agents are operator/cross-tenant, so there is no per-org incident here.

use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::Result;
use crate::observability::metrics::names;
use crate::storage::operator::OperatorRepo;

const TICK: Duration = Duration::from_secs(30);

pub async fn run(repo: OperatorRepo, stale_after: Duration, shutdown: CancellationToken) {
    let mut ticker = interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut stale: HashSet<Uuid> = HashSet::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                if let Err(err) = sweep(&repo, stale_after, &mut stale).await {
                    tracing::warn!(error = %err, "agent health sweep failed");
                }
            }
        }
    }
}

async fn sweep(
    repo: &OperatorRepo,
    stale_after: Duration,
    stale: &mut HashSet<Uuid>,
) -> Result<()> {
    let agents = repo.list_agents().await?;
    let now = Utc::now();
    let mut live: HashSet<Uuid> = HashSet::new();
    for a in &agents {
        live.insert(a.id);
        // Never-reported agents age from creation.
        let since = a.last_seen_at.unwrap_or(a.created_at);
        let age = (now - since).num_seconds().max(0);
        let is_stale = age as u64 > stale_after.as_secs();
        // Emit for every agent (incl. disabled) so a disabled/dark agent's gauge
        // ages to 0 rather than freezing at its last value. (A deleted agent's
        // series can't be retracted via the metrics facade — its `agent_up`
        // freezes; the live `stale` flag on GET /operator/agents is the source
        // of truth there.)
        metrics::gauge!(names::AGENT_LAST_SEEN_AGE, "region" => a.region.clone(), "agent" => a.name.clone())
            .set(age as f64);
        metrics::gauge!(names::AGENT_UP, "region" => a.region.clone(), "agent" => a.name.clone())
            .set(if is_stale { 0.0 } else { 1.0 });
        // Warn only when an *enabled* agent crosses into stale — a disabled one
        // is intentionally dark, not an incident.
        if is_stale && a.enabled {
            if stale.insert(a.id) {
                tracing::warn!(region = %a.region, agent = %a.name, age_secs = age, "regional agent is stale (no check-in)");
            }
        } else {
            stale.remove(&a.id);
        }
    }
    // Forget vanished agents so the log-dedup set can't grow unbounded.
    stale.retain(|id| live.contains(id));
    Ok(())
}
