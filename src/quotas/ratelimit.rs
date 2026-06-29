//! Per-org / per-user request rate limiting.
//!
//! One `governor` direct limiter per `(scope, category)` key in a `DashMap`.
//! Both the org tier and the user tier are checked; whichever is hit first
//! produces the 429. Per-IP limiting is Caddy's job (it sees the real peer);
//! this tier keys on the authenticated subject, never the TCP peer, so a
//! single reverse-proxy hop cannot collapse every tenant into one bucket.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use governor::clock::{Clock, DefaultClock};
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::domain::quota::Plan;
use crate::domain::{OrgId, UserId};

type DirectLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitCategory {
    ApiWrites,
    ApiReads,
    BulkOps,
    TestNow,
    CheckNow,
}

impl RateLimitCategory {
    /// Per-minute quota for this category from the plan. Floored at 1 so a
    /// misconfigured `0` becomes a clean (if strict) limiter rather than a
    /// `NonZeroU32` panic at `Quota::per_minute` (I6).
    fn per_minute(self, plan: &Plan) -> NonZeroU32 {
        let raw = match self {
            Self::ApiWrites => plan.api_writes_per_minute,
            Self::ApiReads => plan.api_reads_per_minute,
            Self::BulkOps => plan.bulk_ops_per_minute,
            Self::TestNow => plan.test_now_per_minute,
            Self::CheckNow => plan.check_now_per_minute,
        };
        let clamped = u32::try_from(raw).unwrap_or(u32::MAX).max(1);
        NonZeroU32::new(clamped).expect("clamped to >= 1")
    }

    fn as_scope(self, tier: &'static str) -> String {
        let c = match self {
            Self::ApiWrites => "api_writes",
            Self::ApiReads => "api_reads",
            Self::BulkOps => "bulk_ops",
            Self::TestNow => "test_now",
            Self::CheckNow => "check_now",
        };
        format!("{tier}_{c}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RateLimitKey {
    Org(OrgId, RateLimitCategory),
    User(UserId, RateLimitCategory),
}

struct Entry {
    limiter: DirectLimiter,
    /// Unix-millis of the last `check`. The janitor evicts entries idle
    /// longer than the threshold so the map stays bounded.
    last_seen: AtomicI64,
}

/// Outcome of a denied check: the scope label and how long until a retry.
pub struct Denied {
    pub scope: String,
    pub retry_after_secs: u32,
}

pub struct RateLimitService {
    limiters: DashMap<RateLimitKey, Arc<Entry>>,
    clock: DefaultClock,
}

impl RateLimitService {
    pub fn new() -> Self {
        Self {
            limiters: DashMap::new(),
            clock: DefaultClock::default(),
        }
    }

    fn now_millis() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// Check one tier. `Ok(())` = allowed; `Err(Denied)` = 429.
    pub fn check(&self, key: RateLimitKey, tier: &'static str, plan: &Plan) -> Result<(), Denied> {
        let category = match &key {
            RateLimitKey::Org(_, c) | RateLimitKey::User(_, c) => *c,
        };
        let entry = self
            .limiters
            .entry(key)
            .or_insert_with(|| {
                let quota = Quota::per_minute(category.per_minute(plan));
                Arc::new(Entry {
                    limiter: RateLimiter::direct(quota),
                    last_seen: AtomicI64::new(Self::now_millis()),
                })
            })
            .clone();
        entry.last_seen.store(Self::now_millis(), Ordering::Relaxed);

        match entry.limiter.check() {
            Ok(()) => Ok(()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(self.clock.now());
                Err(Denied {
                    scope: category.as_scope(tier),
                    retry_after_secs: (wait.as_secs() as u32).max(1),
                })
            }
        }
    }

    /// Evict entries untouched for `idle`. Returns how many were removed.
    /// One indexed pass; called by the janitor only.
    pub fn sweep(&self, idle: Duration) -> usize {
        let cutoff = Self::now_millis() - idle.as_millis() as i64;
        let stale: Vec<RateLimitKey> = self
            .limiters
            .iter()
            .filter(|e| e.value().last_seen.load(Ordering::Relaxed) < cutoff)
            .map(|e| e.key().clone())
            .collect();
        for k in &stale {
            self.limiters.remove(k);
        }
        stale.len()
    }

    /// Live entry count. Used by the janitor regression test to assert the
    /// sweep bounds the map.
    pub fn len(&self) -> usize {
        self.limiters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.limiters.is_empty()
    }

    /// Spawn the idle-entry janitor. Co-located with the service so the
    /// "construct" and "sweep" responsibilities are not separable: the task
    /// exits when `shutdown` fires (same lifetime as every other background
    /// worker), so a refactor cannot silently drop the sweep and leak the map.
    pub fn spawn_janitor(
        self: Arc<Self>,
        cleanup_every: Duration,
        idle_threshold: Duration,
        shutdown: CancellationToken,
    ) {
        tokio::spawn(async move {
            let mut ticker = interval(cleanup_every);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = ticker.tick() => {
                        let removed = self.sweep(idle_threshold);
                        if removed > 0 {
                            tracing::debug!(removed, "rate-limit janitor swept idle entries");
                        }
                    }
                }
            }
        });
    }
}

impl Default for RateLimitService {
    fn default() -> Self {
        Self::new()
    }
}
