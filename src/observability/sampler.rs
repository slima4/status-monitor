use std::sync::Arc;
use std::time::Duration;

use metrics::gauge;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::domain::CheckResult;
use crate::observability::metrics::names;
use crate::scheduler::TargetRegistry;
use crate::worker::WorkerPool;

pub fn spawn(
    pool: Arc<WorkerPool>,
    registry: Arc<TargetRegistry>,
    result_tx: &mpsc::Sender<CheckResult>,
    sample_interval: Duration,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    // WeakSender so the sampler doesn't keep the result channel alive past
    // shutdown — the batcher's rx-drop fallback path needs the channel to
    // close once all real senders are gone.
    let queue_capacity = result_tx.max_capacity();
    let tx = result_tx.downgrade();
    tokio::spawn(run(
        pool,
        registry,
        tx,
        queue_capacity,
        sample_interval,
        shutdown,
    ))
}

async fn run(
    pool: Arc<WorkerPool>,
    registry: Arc<TargetRegistry>,
    result_tx: mpsc::WeakSender<CheckResult>,
    queue_capacity: usize,
    sample_interval: Duration,
    shutdown: CancellationToken,
) {
    let mut ticker = interval(sample_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let g_in_flight = gauge!(names::WORKERS_IN_FLIGHT);
    let g_breakers_open = gauge!(names::BREAKERS_OPEN);
    let g_targets = gauge!(names::TARGETS_TOTAL);
    let g_queue_depth = gauge!(names::RESULT_QUEUE_DEPTH);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                g_in_flight.set(pool.in_flight() as f64);
                g_breakers_open.set(pool.open_breakers() as f64);
                g_targets.set(registry.len() as f64);
                let depth = match result_tx.upgrade() {
                    Some(tx) => queue_capacity.saturating_sub(tx.capacity()),
                    None => 0,
                };
                g_queue_depth.set(depth as f64);
            }
        }
    }
}
