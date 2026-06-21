//! Configured-monitor and active-user counts from Postgres, refreshed on a
//! slow cadence and scrape-cached so request load never reaches Postgres.

use std::time::Duration;

use anyhow::Context;
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::domain::CheckSpec;
use crate::error::Result;
use crate::observability::metrics::names;

const TICK: Duration = Duration::from_secs(60);

pub async fn run(pg: PgPool, shutdown: CancellationToken) {
    let mut ticker = interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                if let Err(err) = sweep(&pg).await {
                    tracing::warn!(?err, "inventory gauge sweep failed; serving last values");
                }
            }
        }
    }
}

async fn sweep(pg: &PgPool) -> Result<()> {
    let by_kind: Vec<(String, i64)> = sqlx::query_as(
        "SELECT kind, count(*) FROM targets WHERE enabled GROUP BY kind \
             /* SAFE: operator-wide inventory gauge, counts every org by design */",
    )
    .fetch_all(pg)
    .await
    .context("count enabled targets by kind")?;
    // Emit every kind so one that drains to zero reports 0, not its last value.
    for kind in CheckSpec::ALL_KINDS {
        let n = by_kind
            .iter()
            .find(|(k, _)| k == kind)
            .map_or(0, |(_, n)| *n);
        metrics::gauge!(names::TARGETS_ENABLED, "kind" => kind).set(n as f64);
    }

    let active_users: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE deleted_at IS NULL")
            .fetch_one(pg)
            .await
            .context("count active users")?;
    metrics::gauge!(names::USERS_ACTIVE).set(active_users as f64);

    Ok(())
}
