use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep};
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
    if !sleep_or_cancel(jitter(base, jitter_pct), &shutdown).await {
        return;
    }

    let mut tick = interval_at(Instant::now() + base, base);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    dispatch(&pool, &target);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        if !sleep_or_cancel(jitter(base, jitter_pct), &shutdown).await {
            return;
        }
        dispatch(&pool, &target);
    }
}

async fn sleep_or_cancel(duration: Duration, shutdown: &CancellationToken) -> bool {
    if duration.is_zero() {
        return !shutdown.is_cancelled();
    }
    tokio::select! {
        _ = shutdown.cancelled() => false,
        _ = sleep(duration) => true,
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
