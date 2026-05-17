use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::SchedulerConfig;
use crate::domain::Target;
use crate::error::Result;
use crate::scheduler::registry::TargetRegistry;
use crate::worker::{CheckTask, WorkerPool};

pub struct Scheduler {
    registry: Arc<TargetRegistry>,
    pool: Arc<WorkerPool>,
    cfg: SchedulerConfig,
    tasks: Arc<DashMap<Uuid, TargetTaskHandle>>,
}

struct TargetTaskHandle {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl Scheduler {
    pub fn new(registry: Arc<TargetRegistry>, pool: Arc<WorkerPool>, cfg: SchedulerConfig) -> Self {
        Self {
            registry,
            pool,
            cfg,
            tasks: Arc::new(DashMap::new()),
        }
    }

    pub fn active_tasks(&self) -> usize {
        self.tasks.len()
    }

    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) -> Result<()> {
        let mut refresh = tokio::time::interval(Duration::from_secs(
            self.cfg.target_refresh_interval_secs.max(1),
        ));
        refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);

        if let Err(err) = self.tick_once().await {
            tracing::error!(?err, "initial registry refresh failed");
        }

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("scheduler shutting down");
                    break;
                }
                _ = refresh.tick() => {
                    if let Err(err) = self.tick_once().await {
                        tracing::error!(?err, "registry refresh failed");
                    }
                }
            }
        }

        self.cancel_all();
        Ok(())
    }

    async fn tick_once(&self) -> Result<()> {
        let diff = self.registry.refresh().await?;
        for id in diff.removed {
            if let Some((_, handle)) = self.tasks.remove(&id) {
                handle.cancel.cancel();
            }
        }
        for target in diff.updated {
            if let Some((_, handle)) = self.tasks.remove(&target.id) {
                handle.cancel.cancel();
            }
            self.spawn_target(target);
        }
        for target in diff.added {
            self.spawn_target(target);
        }
        Ok(())
    }

    fn spawn_target(&self, target: Arc<Target>) {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let pool = self.pool.clone();
        let jitter_pct = self.cfg.jitter_pct;
        let id = target.id;

        let handle = tokio::spawn(async move {
            run_target_loop(target, pool, jitter_pct, token).await;
        });
        self.tasks.insert(id, TargetTaskHandle { cancel, handle });
    }

    fn cancel_all(&self) {
        let ids: Vec<Uuid> = self.tasks.iter().map(|e| *e.key()).collect();
        for id in ids {
            if let Some((_, h)) = self.tasks.remove(&id) {
                h.cancel.cancel();
                drop(h.handle);
            }
        }
    }
}

async fn run_target_loop(
    target: Arc<Target>,
    pool: Arc<WorkerPool>,
    jitter_pct: u8,
    shutdown: CancellationToken,
) {
    let base = target.interval;

    // Check immediately so a freshly-scheduled target reports up/down right
    // away instead of staying blank until the first interval elapses.
    dispatch(&pool, &target);

    // Validation enforces a per-plan interval floor (>= 1s), so a zero
    // interval — which would panic `interval_at` — is structurally impossible.
    debug_assert!(!base.is_zero(), "target interval must be non-zero");

    // Jitter is a one-time phase offset: it spreads targets across the window
    // (thundering-herd protection) while the fixed-cadence timer keeps every
    // subsequent tick on a steady schedule. Re-drawing jitter each cycle is
    // what made the observed interval visibly drift.
    let start = Instant::now() + base + jitter(base, jitter_pct);
    let mut tick = interval_at(start, base);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        dispatch(&pool, &target);
    }
}

fn dispatch(pool: &WorkerPool, target: &Arc<Target>) {
    pool.dispatch(CheckTask {
        target: target.clone(),
    });
}

fn jitter(base: Duration, jitter_pct: u8) -> Duration {
    if jitter_pct == 0 || base.is_zero() {
        return Duration::ZERO;
    }
    let span = base.as_millis() as u64 * jitter_pct as u64 / 100;
    if span == 0 {
        return Duration::ZERO;
    }
    let drawn = Duration::from_millis(fastrand::u64(0..=span));
    drawn.min(base / 2)
}

#[cfg(test)]
mod tests {
    use super::jitter;
    use std::time::Duration;

    #[test]
    fn jitter_is_zero_when_pct_zero() {
        assert_eq!(jitter(Duration::from_secs(60), 0), Duration::ZERO);
    }

    #[test]
    fn jitter_is_zero_when_base_zero() {
        assert_eq!(jitter(Duration::ZERO, 50), Duration::ZERO);
    }

    #[test]
    fn jitter_is_zero_when_span_rounds_down_to_zero() {
        // 5ms * 10% = 0ms span → no usable spread, must collapse to zero
        // rather than panic on `fastrand::u64(0..=0)`.
        assert_eq!(jitter(Duration::from_millis(5), 10), Duration::ZERO);
    }

    #[test]
    fn jitter_offset_stays_within_span() {
        // The phase offset must never exceed the span (pct of base), and must
        // actually spread (not silently collapse to zero — that would defeat
        // thundering-herd protection). Sample heavily since the draw is random.
        let base = Duration::from_millis(1000);
        let pct = 10u8;
        let span = Duration::from_millis(base.as_millis() as u64 * pct as u64 / 100);
        let mut max_seen = Duration::ZERO;
        for _ in 0..10_000 {
            let j = jitter(base, pct);
            assert!(j <= span, "{j:?} exceeded span {span:?}");
            max_seen = max_seen.max(j);
        }
        assert!(max_seen > Duration::ZERO, "jitter never produced a spread");
    }

    #[test]
    fn jitter_offset_is_clamped_to_half_base_for_large_pct() {
        // pct=100 ⇒ span=base, so only the `.min(base / 2)` clamp keeps the
        // offset from pushing the timer's first tick past a whole period.
        let base = Duration::from_millis(200);
        for _ in 0..10_000 {
            assert!(jitter(base, 100) <= base / 2);
        }
    }
}
