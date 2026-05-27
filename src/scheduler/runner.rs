use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use dashmap::DashMap;
use metrics::{Counter, Gauge, counter, gauge, histogram};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::SchedulerConfig;
use crate::error::Result;
use crate::observability::metrics::names;
use crate::scheduler::registry::{ScheduledTarget, TargetRegistry};
use crate::worker::{CheckTask, WorkerPool};

/// Fixed sweep cadence — decoupled from registry-refresh so a DB outage or
/// an operator-tuned refresh interval doesn't delay memory reclamation.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Upper bound on shutdown drain. Target loops exit on the next select poll
/// after their cancel token fires, so well-behaved tasks return in milliseconds.
/// The budget exists to bound shutdown on a stuck task (e.g. one spinning in a
/// non-cancel-aware sync block) rather than hanging the process indefinitely.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Backoff cap on consecutive refresh failures: next attempt waits at most
/// `base × REFRESH_BACKOFF_CAP_MULTIPLIER` seconds. Bounds recovery latency
/// when the upstream DB returns — at default base=30s and cap=10, steady-state
/// retry cadence under sustained outage is 5 minutes, with recovery noticed
/// within the same window.
const REFRESH_BACKOFF_CAP_MULTIPLIER: u64 = 10;

static REFRESH_FAILED: LazyLock<Counter> =
    LazyLock::new(|| counter!(names::SCHEDULER_REFRESH_FAILED));
static CONSECUTIVE_REFRESH_FAILURES: LazyLock<Gauge> =
    LazyLock::new(|| gauge!(names::SCHEDULER_CONSECUTIVE_REFRESH_FAILURES));

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
        debug_assert!(
            self.cfg.target_refresh_interval_secs >= 1,
            "target_refresh_interval_secs must be validated >= 1 before Scheduler::run",
        );
        let mut refresh =
            tokio::time::interval(Duration::from_secs(self.cfg.target_refresh_interval_secs));
        refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut consecutive_failures: u32 = 0;

        // Initial tick made cancel-aware so an operator-triggered shutdown
        // during a slow PG handshake doesn't block boot for the full pool
        // timeout. If shutdown fires here, skip the loop entirely.
        let initial = tokio::select! {
            _ = shutdown.cancelled() => None,
            r = self.tick_once() => Some(r),
        };
        match initial {
            None => {
                tracing::info!("scheduler shutting down before initial refresh");
                self.cancel_all().await;
                return Ok(());
            }
            Some(r) => self.handle_refresh_result(&mut consecutive_failures, &mut refresh, r),
        }

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("scheduler shutting down");
                    break;
                }
                _ = refresh.tick() => {
                    // tick_once awaits PG; wrap in a nested select so the
                    // shutdown signal is observable during a slow handshake.
                    let outcome = tokio::select! {
                        _ = shutdown.cancelled() => None,
                        r = self.tick_once() => Some(r),
                    };
                    match outcome {
                        None => break,
                        Some(r) => self.handle_refresh_result(
                            &mut consecutive_failures,
                            &mut refresh,
                            r,
                        ),
                    }
                }
                _ = sweep.tick() => {
                    self.sweep_once();
                }
            }
        }

        self.cancel_all().await;
        Ok(())
    }

    fn handle_refresh_result(
        &self,
        consecutive_failures: &mut u32,
        refresh: &mut tokio::time::Interval,
        result: Result<()>,
    ) {
        match result {
            Ok(()) => {
                if *consecutive_failures > 0 {
                    tracing::info!(
                        prior_consecutive_failures = *consecutive_failures,
                        "registry refresh recovered",
                    );
                    CONSECUTIVE_REFRESH_FAILURES.set(0.0);
                    *consecutive_failures = 0;
                }
            }
            Err(err) => {
                *consecutive_failures = consecutive_failures.saturating_add(1);
                REFRESH_FAILED.increment(1);
                CONSECUTIVE_REFRESH_FAILURES.set(*consecutive_failures as f64);
                let base = self.cfg.target_refresh_interval_secs;
                let delay_secs = backoff_delay_secs(base, *consecutive_failures);
                tracing::error!(
                    ?err,
                    consecutive_failures = *consecutive_failures,
                    next_attempt_in_secs = delay_secs,
                    "registry refresh failed",
                );
                refresh.reset_after(Duration::from_secs(delay_secs));
            }
        }
    }

    async fn tick_once(&self) -> Result<()> {
        let started = std::time::Instant::now();
        let result = self.registry.refresh().await;
        // Record on both Ok and Err so Grafana can see "we got slow" AND
        // "we got slow then timed out" — both inputs to the alerting signal
        // that drives the future incremental-sync work.
        histogram!(names::SCHEDULER_REFRESH_DURATION_MS)
            .record(started.elapsed().as_millis() as f64);
        let diff = result?;
        for id in diff.removed {
            if let Some((_, handle)) = self.tasks.remove(&id) {
                handle.cancel.cancel();
            }
        }
        for st in diff.updated {
            if let Some((_, handle)) = self.tasks.remove(&st.target.id) {
                handle.cancel.cancel();
            }
            self.spawn_target(st);
        }
        for st in diff.added {
            self.spawn_target(st);
        }
        Ok(())
    }

    fn sweep_once(&self) {
        let evicted_throttle = self.pool.host_throttle().sweep();
        let evicted_breakers = self.pool.sweep_breakers();
        let evicted_singleflight = self.pool.domain_expiry_runtime().singleflight.sweep();
        if evicted_throttle > 0 || evicted_breakers > 0 || evicted_singleflight > 0 {
            tracing::debug!(
                evicted_throttle,
                evicted_breakers,
                evicted_singleflight,
                "scheduler swept idle entries"
            );
        }
    }

    fn spawn_target(&self, st: ScheduledTarget) {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let pool = self.pool.clone();
        let id = st.target.id;

        let handle = tokio::spawn(async move {
            run_target_loop(st, pool, token).await;
        });
        self.tasks.insert(id, TargetTaskHandle { cancel, handle });
    }

    async fn cancel_all(&self) {
        let ids: Vec<Uuid> = self.tasks.iter().map(|e| *e.key()).collect();
        let mut handles: Vec<(Uuid, JoinHandle<()>)> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some((_, h)) = self.tasks.remove(&id) {
                h.cancel.cancel();
                handles.push((id, h.handle));
            }
        }
        drain_handles(handles, SHUTDOWN_DRAIN_TIMEOUT).await;
    }
}

/// Exponential backoff for consecutive refresh failures. Doubles each
/// failure until it hits the cap (`base × REFRESH_BACKOFF_CAP_MULTIPLIER`)
/// — bounded so the scheduler notices PG recovery within one cap-window.
fn backoff_delay_secs(base_secs: u64, consecutive_failures: u32) -> u64 {
    let shift = consecutive_failures.saturating_sub(1).min(u32::BITS - 1);
    let mult = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let mult = mult.min(REFRESH_BACKOFF_CAP_MULTIPLIER);
    base_secs.saturating_mul(mult)
}

/// Awaits target-task handles after their cancel tokens have fired. Bounded
/// by `timeout`: on expiry, remaining handles are dropped (detached), matching
/// the pre-fix behaviour as a fallback — never blocks shutdown forever.
async fn drain_handles(handles: Vec<(Uuid, JoinHandle<()>)>, timeout: Duration) {
    let total = handles.len();
    if total == 0 {
        return;
    }
    tracing::info!(target_tasks = total, "draining target tasks on shutdown");
    let started = std::time::Instant::now();

    let drain = async {
        for (id, h) in handles {
            match h.await {
                Ok(()) => {}
                Err(err) if err.is_panic() => {
                    tracing::error!(
                        target_id = %id,
                        error = %err,
                        "target task panicked during shutdown",
                    );
                }
                Err(_) => {}
            }
        }
    };

    match tokio::time::timeout(timeout, drain).await {
        Ok(()) => tracing::info!(
            target_tasks = total,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "target tasks drained",
        ),
        Err(_) => tracing::warn!(
            target_tasks = total,
            timeout_secs = timeout.as_secs(),
            "scheduler shutdown drain timed out — orphaning remaining tasks",
        ),
    }
}

async fn run_target_loop(st: ScheduledTarget, pool: Arc<WorkerPool>, shutdown: CancellationToken) {
    let base = st.target.interval;

    // Check immediately so a freshly-scheduled target reports up/down right
    // away instead of staying blank until the first interval elapses.
    dispatch(&pool, &st);

    // Validation enforces a per-plan interval floor (>= 1s), so a zero
    // interval — which would panic `interval_at` — is structurally impossible.
    debug_assert!(!base.is_zero(), "target interval must be non-zero");

    // Deterministic per-target phase offset across the full interval window.
    // N targets sharing a host get spread over [0, interval) with expected
    // gap interval/N — eliminates the random-collision starvation pattern
    // a small random jitter window left on the per-host throttle.
    let start = Instant::now() + base + stagger_offset(st.target.id, base);
    let mut tick = interval_at(start, base);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        dispatch(&pool, &st);
    }
}

fn dispatch(pool: &WorkerPool, st: &ScheduledTarget) {
    pool.dispatch(CheckTask {
        target: st.target.clone(),
        org_id: st.org_id,
        host_key: st.host_key.clone(),
        breaker_key: st.breaker_key.clone(),
        rdap_tld: st.rdap_tld.clone(),
    });
}

fn stagger_offset(id: Uuid, interval: Duration) -> Duration {
    let interval_ms = interval.as_millis() as u64;
    if interval_ms == 0 {
        return Duration::ZERO;
    }
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    Duration::from_millis(h.finish() % interval_ms)
}

#[cfg(test)]
mod tests {
    use super::{
        REFRESH_BACKOFF_CAP_MULTIPLIER, backoff_delay_secs, drain_handles, stagger_offset,
    };
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    #[test]
    fn stagger_offset_is_deterministic() {
        let id = Uuid::from_u128(0x1234_5678_90AB_CDEF_1234_5678_90AB_CDEF);
        let base = Duration::from_secs(60);
        let a = stagger_offset(id, base);
        let b = stagger_offset(id, base);
        assert_eq!(a, b, "same id+interval must yield identical offset");
    }

    #[test]
    fn stagger_offset_stays_within_interval() {
        let base = Duration::from_secs(60);
        for n in 0..1000u128 {
            let id = Uuid::from_u128(n);
            let o = stagger_offset(id, base);
            assert!(o < base, "offset {o:?} must be < interval {base:?}");
        }
    }

    #[test]
    fn stagger_offset_zero_interval_is_safe() {
        assert_eq!(stagger_offset(Uuid::nil(), Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn stagger_offset_distributes_uniformly() {
        // 1000 distinct UUIDs hashed into 10 buckets over a 60s interval.
        // Each bucket should receive ~100 ± a generous tolerance — confirms
        // the hash spreads the host-shared targets across the window instead
        // of clumping them into the same throttle contention slot.
        let base = Duration::from_secs(60);
        let bucket_ms = base.as_millis() as u64 / 10;
        let mut buckets = [0u32; 10];
        for n in 0..1000u128 {
            let o = stagger_offset(Uuid::from_u128(n + 0xDEAD_BEEF_0000), base);
            let idx = (o.as_millis() as u64 / bucket_ms).min(9) as usize;
            buckets[idx] += 1;
        }
        for (i, &c) in buckets.iter().enumerate() {
            assert!(
                (60..=160).contains(&c),
                "bucket {i} got {c} hits — distribution is clumped: {buckets:?}",
            );
        }
    }

    #[tokio::test]
    async fn drain_handles_returns_immediately_when_no_tasks() {
        let started = std::time::Instant::now();
        drain_handles(Vec::new(), Duration::from_secs(5)).await;
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn drain_handles_awaits_well_behaved_tasks() {
        // Three tasks that exit promptly on cancel. Drain must block until all
        // have observably returned, not just detach their JoinHandles.
        let mut handles = Vec::new();
        let exited = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..3 {
            let tok = CancellationToken::new();
            let tok_c = tok.clone();
            let exited_c = exited.clone();
            let h = tokio::spawn(async move {
                tok_c.cancelled().await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                exited_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            tok.cancel();
            handles.push((Uuid::nil(), h));
        }
        let started = std::time::Instant::now();
        drain_handles(handles, Duration::from_secs(5)).await;
        assert_eq!(
            exited.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "all tasks must have run to completion before drain returned",
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "drain hung: {:?}",
            started.elapsed(),
        );
    }

    #[tokio::test]
    async fn drain_handles_times_out_on_stuck_task() {
        // Task ignores its cancel and never exits. Drain must bound shutdown
        // by the timeout, not block forever.
        let h = tokio::spawn(async { std::future::pending::<()>().await });
        let started = std::time::Instant::now();
        drain_handles(vec![(Uuid::nil(), h)], Duration::from_millis(50)).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50),
            "drain returned before timeout: {elapsed:?}",
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "timeout not enforced: {elapsed:?}",
        );
    }

    #[test]
    fn backoff_walks_the_expected_curve_with_default_base() {
        // Base 30s, cap multiplier 10 → steady state at 300s.
        let base = 30u64;
        assert_eq!(backoff_delay_secs(base, 1), 30);
        assert_eq!(backoff_delay_secs(base, 2), 60);
        assert_eq!(backoff_delay_secs(base, 3), 120);
        assert_eq!(backoff_delay_secs(base, 4), 240);
        // 2^4 = 16, capped at REFRESH_BACKOFF_CAP_MULTIPLIER = 10.
        assert_eq!(
            backoff_delay_secs(base, 5),
            base * REFRESH_BACKOFF_CAP_MULTIPLIER
        );
        assert_eq!(
            backoff_delay_secs(base, 100),
            base * REFRESH_BACKOFF_CAP_MULTIPLIER
        );
    }

    #[test]
    fn backoff_does_not_panic_at_extremes() {
        // Saturating arithmetic guards both the shift and the multiply.
        assert!(backoff_delay_secs(u64::MAX, 1) >= 1);
        assert!(backoff_delay_secs(u64::MAX, u32::MAX) >= 1);
        assert_eq!(backoff_delay_secs(1, 0), 1);
    }

    #[tokio::test]
    async fn drain_handles_continues_past_panicked_task() {
        // A panicked task surfaces as a JoinError::Panic. Drain must log it
        // and keep awaiting the rest — a single panicky target can't strand
        // a well-behaved peer.
        let h_panic = tokio::spawn(async { panic!("test panic") });
        let tok = CancellationToken::new();
        let tok_c = tok.clone();
        let exited = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let exited_c = exited.clone();
        let h_ok = tokio::spawn(async move {
            tok_c.cancelled().await;
            exited_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        tok.cancel();
        drain_handles(
            vec![(Uuid::nil(), h_panic), (Uuid::nil(), h_ok)],
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(exited.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
