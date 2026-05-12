use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Connection pool counters.
///
/// `alive` is incremented when the custom connector successfully establishes a new connection
/// and decremented when the connection IO is dropped. `active_requests` is held by an
/// [`ActiveGuard`] for the lifetime of an in-flight HTTP request — at zero open requests,
/// every alive connection is idle.
///
/// `idle ≈ alive - active_requests`. The approximation is exact for HTTP/1 (one request per
/// connection at a time); on HTTP/2 a single connection can serve many concurrent streams so
/// `active_requests` may exceed `alive` and `idle()` clamps to zero. Document this behaviour
/// rather than emit nonsense.
#[derive(Default)]
pub struct PoolStats {
    pub alive: AtomicU64,
    pub active_requests: AtomicU64,
}

impl PoolStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn idle(&self) -> u64 {
        self.alive
            .load(Ordering::Relaxed)
            .saturating_sub(self.active_requests.load(Ordering::Relaxed))
    }

    pub fn active(&self) -> u64 {
        self.active_requests.load(Ordering::Relaxed)
    }

    pub fn inflight_guard(self: &Arc<Self>) -> ActiveGuard {
        self.active_requests.fetch_add(1, Ordering::Relaxed);
        ActiveGuard {
            stats: self.clone(),
        }
    }
}

pub struct ActiveGuard {
    stats: Arc<PoolStats>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.stats.active_requests.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct AliveGuard {
    stats: Arc<PoolStats>,
}

impl AliveGuard {
    pub fn new(stats: Arc<PoolStats>) -> Self {
        stats.alive.fetch_add(1, Ordering::Relaxed);
        Self { stats }
    }
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.stats.alive.fetch_sub(1, Ordering::Relaxed);
    }
}
