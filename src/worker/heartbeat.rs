//! Heartbeat evaluation: a scheduled check that reads in-memory ping state
//! instead of probing the network. `/ping/{token}` records into
//! [`HeartbeatRuntime`]; the executor compares the anchor's age against the
//! spec's `period + grace`.
//!
//! Postgres is the source of truth: the scheduler's refresh tick reconciles
//! this cache from it (add, max-merge, prune) before dispatching a new target,
//! so restarts, re-arms, org restores, and multi-replica ingest converge
//! within one refresh interval.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};
use std::collections::HashMap;
use std::num::NonZeroU32;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, HeartbeatCheck};

/// Per-token accepted-ping rate + burst; extra pings 429. Keyed pre-resolve so
/// rejected pings never reach Postgres.
const PING_PER_SEC: u32 = 1;
const PING_BURST: u32 = 10;

type PingLimiter = RateLimiter<u128, DashMapStateStore<u128>, DefaultClock>;

/// Shared main↔worker heartbeat state: the anchor cache the executor reads,
/// plus the ingest rate limiter.
pub struct HeartbeatRuntime {
    anchors: DashMap<Uuid, DateTime<Utc>>,
    ping_limiter: PingLimiter,
}

impl Default for HeartbeatRuntime {
    fn default() -> Self {
        let quota = Quota::per_second(NonZeroU32::new(PING_PER_SEC).expect("nonzero"))
            .allow_burst(NonZeroU32::new(PING_BURST).expect("nonzero"));
        Self {
            anchors: DashMap::new(),
            ping_limiter: RateLimiter::dashmap(quota),
        }
    }
}

impl HeartbeatRuntime {
    pub fn anchor(&self, id: Uuid) -> Option<DateTime<Utc>> {
        self.anchors.get(&id).map(|e| *e)
    }

    pub fn set_anchor(&self, id: Uuid, at: DateTime<Utc>) {
        self.anchors.insert(id, at);
    }

    /// Reconcile the cache against the Postgres snapshot: prune ids it no
    /// longer has, max-merge per id so a ping accepted mid-read never
    /// regresses, and sweep idle rate-limiter keys.
    pub fn sync_anchors(&self, fresh: HashMap<Uuid, DateTime<Utc>>) {
        self.anchors.retain(|id, _| fresh.contains_key(id));
        for (id, at) in fresh {
            self.anchors
                .entry(id)
                .and_modify(|cur| {
                    if at > *cur {
                        *cur = at;
                    }
                })
                .or_insert(at);
        }
        self.ping_limiter.retain_recent();
    }

    /// GCRA admission for one inbound ping, keyed on the presented token's
    /// hash. A rejected ping reserves nothing.
    pub fn allow_ping(&self, token_key: u128) -> bool {
        self.ping_limiter.check_key(&token_key).is_ok()
    }
}

pub fn execute_heartbeat_check(
    target_id: Uuid,
    org_id: Uuid,
    check: &HeartbeatCheck,
    runtime: &HeartbeatRuntime,
) -> CheckResult {
    let now = Utc::now();
    let Some(anchor) = runtime.anchor(target_id) else {
        // Sub-refresh-interval race at worst (refresh syncs anchors before
        // dispatch); Error resolves next tick rather than flapping a false Down.
        return CheckResult::error(
            target_id,
            org_id,
            "heartbeat state unavailable on this node",
        );
    };
    let age = now.signed_duration_since(anchor);
    // A future anchor (ingest/eval clock skew) is a fresh ping, never Down.
    let allowed =
        chrono::Duration::from_std(check.period + check.grace).unwrap_or(chrono::Duration::MAX);
    let status = if age <= allowed {
        CheckStatus::Up
    } else {
        CheckStatus::Down
    };
    CheckResult {
        target_id,
        org_id,
        timestamp: now,
        status,
        duration_ms: 0,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: None,
        response_code: None,
        response_size: None,
        error: (status == CheckStatus::Down).then(|| {
            format!(
                "no ping for {}s, expected every {}s (+{}s grace)",
                age.num_seconds().max(0),
                check.period.as_secs(),
                check.grace.as_secs()
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn check(period_s: u64, grace_s: u64) -> HeartbeatCheck {
        HeartbeatCheck {
            period: Duration::from_secs(period_s),
            grace: Duration::from_secs(grace_s),
        }
    }

    #[test]
    fn fresh_ping_is_up() {
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now());
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 30), &rt);
        assert_eq!(r.status, CheckStatus::Up);
        assert!(r.error.is_none());
    }

    #[test]
    fn stale_past_period_plus_grace_is_down() {
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() - chrono::Duration::seconds(120));
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 30), &rt);
        assert_eq!(r.status, CheckStatus::Down);
        let err = r.error.expect("down carries a reason");
        assert!(err.contains("expected every 60s"), "{err}");
    }

    #[test]
    fn within_grace_is_still_up() {
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() - chrono::Duration::seconds(80));
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 30), &rt);
        assert_eq!(r.status, CheckStatus::Up);
    }

    #[test]
    fn missing_anchor_is_error_not_down() {
        let rt = HeartbeatRuntime::default();
        let r = execute_heartbeat_check(Uuid::new_v4(), Uuid::new_v4(), &check(60, 0), &rt);
        assert_eq!(r.status, CheckStatus::Error);
    }

    #[test]
    fn future_anchor_is_up() {
        // Clock skew between the ingest write and evaluation must not flap.
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() + chrono::Duration::seconds(5));
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 0), &rt);
        assert_eq!(r.status, CheckStatus::Up);
    }

    #[test]
    fn ping_rate_limits_after_burst_and_isolates_tokens() {
        let rt = HeartbeatRuntime::default();
        let mut accepted = 0;
        while rt.allow_ping(7) {
            accepted += 1;
            assert!(accepted < 100, "GCRA never rejected");
        }
        assert!(accepted >= 10, "burst allowance too small: {accepted}");
        assert!(rt.allow_ping(8), "a second token has its own budget");
    }

    #[test]
    fn sync_anchors_prunes_max_merges_and_inserts() {
        let rt = HeartbeatRuntime::default();
        let kept = Uuid::new_v4();
        let gone = Uuid::new_v4();
        let added = Uuid::new_v4();
        let newer = Utc::now();
        let older = newer - chrono::Duration::hours(1);

        rt.set_anchor(kept, newer);
        rt.set_anchor(gone, newer);
        rt.sync_anchors(HashMap::from([(kept, older), (added, older)]));

        assert_eq!(rt.anchor(kept), Some(newer), "max-merge, never regress");
        assert_eq!(rt.anchor(added), Some(older), "snapshot-only id inserted");
        assert!(rt.anchor(gone).is_none(), "absent-from-snapshot id pruned");

        // A newer snapshot value (a re-arm on enable) advances the cache.
        rt.sync_anchors(HashMap::from([(
            kept,
            newer + chrono::Duration::seconds(5),
        )]));
        assert_eq!(rt.anchor(kept), Some(newer + chrono::Duration::seconds(5)));
    }

    #[test]
    fn zero_grace_within_period_is_up() {
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() - chrono::Duration::seconds(30));
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 0), &rt);
        assert_eq!(r.status, CheckStatus::Up);
    }

    #[test]
    fn zero_grace_past_period_is_down() {
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() - chrono::Duration::seconds(90));
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 0), &rt);
        assert_eq!(r.status, CheckStatus::Down);
    }

    #[test]
    fn just_inside_period_plus_grace_is_up() {
        // One second short of the period+grace window still counts as up.
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() - chrono::Duration::seconds(89));
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(60, 30), &rt);
        assert_eq!(r.status, CheckStatus::Up);
    }

    #[test]
    fn max_period_does_not_overflow_the_window() {
        // 30-day period + grace must convert without saturating to a false Down.
        let rt = HeartbeatRuntime::default();
        let id = Uuid::new_v4();
        rt.set_anchor(id, Utc::now() - chrono::Duration::days(1));
        let month = 30 * 24 * 3600;
        let r = execute_heartbeat_check(id, Uuid::new_v4(), &check(month, month), &rt);
        assert_eq!(r.status, CheckStatus::Up);
    }
}
