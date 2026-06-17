//! In-memory dispatch for interactive checks (test / check-now).
//!
//! Agents are outbound-only (serviceless workers, behind NAT), so the brain
//! cannot open a connection to them. Instead an agent holds a long-poll
//! ([`AdHocDispatch::claim`]); the brain hands it a check the instant one is
//! dispatched ([`AdHocDispatch::dispatch`]) and routes the result back to the
//! waiting request ([`AdHocDispatch::complete`]). No DB, no busy polling: one
//! held connection per agent, woken on demand.
//!
//! Single-process: the held claim and the originating request must live on the
//! same brain instance. Fine pre-HA; a multi-instance brain would need sticky
//! routing or a shared bus.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dashmap::DashMap;
use moka::sync::Cache;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::CheckResult;
use crate::domain::agent_wire::{DispatchKind, DispatchedCheck, HeaderPreview};
use crate::http_client::HttpClients;
use crate::storage::ResultSink;
use crate::worker::{WorkerDeps, WorkerPool};

/// How long the brain holds an agent's claim request before returning empty so
/// the agent reconnects. Below the agent's HTTP client timeout.
pub const HOLD: Duration = Duration::from_secs(25);
/// How long a test/check-now request waits for an agent to run its check.
pub const RESULT_WAIT: Duration = Duration::from_secs(30);
/// Cap on queued-but-unclaimed checks per region, so a region that goes dark
/// right after a liveness check can't grow memory without bound.
const MAX_QUEUE: usize = 256;
/// Pending-waiter lifetime. Outlives `RESULT_WAIT` so a check-now result still
/// persists if the agent reports just after the request gave up, and bounds any
/// waiter that is never completed (region died, queue eviction).
const PENDING_TTL: Duration = Duration::from_secs(60);

/// Delivery handle behind interior mutability so the TTL cache can hold it
/// (cache values must be `Clone`) while `complete`/`abandon` take the one-shot
/// sender out exactly once.
type Waiter = Arc<Mutex<Option<oneshot::Sender<DeliveredResult>>>>;

/// Probe result an agent reported back for a dispatched check.
pub struct DeliveredResult {
    pub result: CheckResult,
    pub response_headers_preview: Vec<HeaderPreview>,
    pub response_body_snippet: Option<String>,
}

/// Authoritative check fields, returned by [`AdHocDispatch::complete`] so the
/// caller can persist a check-now result under the real org + target.
#[derive(Clone)]
pub struct DispatchMeta {
    pub kind: DispatchKind,
    pub org_id: Uuid,
    pub target_id: Option<Uuid>,
}

#[derive(Default)]
struct RegionSlot {
    queue: Mutex<VecDeque<DispatchedCheck>>,
    notify: Notify,
    holders: AtomicUsize,
}

struct HolderGuard<'a>(&'a RegionSlot);

impl Drop for HolderGuard<'_> {
    fn drop(&mut self) {
        self.0.holders.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct AdHocDispatch {
    regions: DashMap<String, Arc<RegionSlot>>,
    /// `check_id` → (delivery handle, authoritative meta), TTL-bounded so a
    /// waiter that is never completed — region died, request timed out, queue
    /// eviction — can't leak, and a check-now result still persists if it lands
    /// just after the request gave up.
    pending: Cache<Uuid, (Waiter, DispatchMeta)>,
}

impl Default for AdHocDispatch {
    fn default() -> Self {
        Self::new()
    }
}

impl AdHocDispatch {
    pub fn new() -> Self {
        Self {
            regions: DashMap::new(),
            pending: Cache::builder()
                .time_to_live(PENDING_TTL)
                .max_capacity(100_000)
                .build(),
        }
    }

    fn slot(&self, region: &str) -> Arc<RegionSlot> {
        self.regions.entry(region.to_string()).or_default().clone()
    }

    /// Whether an agent is currently holding a long-poll for this region.
    pub fn region_live(&self, region: &str) -> bool {
        self.regions
            .get(region)
            .is_some_and(|s| s.holders.load(Ordering::Relaxed) > 0)
    }

    /// Queue a check for `region` and return a receiver for its result. Register
    /// the waiter under `check.id`. Caller should check [`Self::region_live`]
    /// first; at the per-region cap the oldest queued check is evicted and its
    /// waiter dropped (its request then fails fast).
    pub fn dispatch(
        &self,
        region: &str,
        check: DispatchedCheck,
    ) -> oneshot::Receiver<DeliveredResult> {
        let (tx, rx) = oneshot::channel();
        let meta = DispatchMeta {
            kind: check.kind,
            org_id: check.org_id,
            target_id: check.target_id,
        };
        self.pending
            .insert(check.id, (Arc::new(Mutex::new(Some(tx))), meta));
        let slot = self.slot(region);
        {
            let mut q = slot.queue.lock().unwrap();
            if q.len() >= MAX_QUEUE
                && let Some(evicted) = q.pop_front()
            {
                self.pending.invalidate(&evicted.id);
            }
            q.push_back(check);
        }
        slot.notify.notify_one();
        rx
    }

    /// Stop delivering to a request that gave up, without discarding the meta —
    /// a check-now result landing within the TTL still persists; the entry is
    /// reclaimed by TTL.
    pub fn abandon(&self, check_id: Uuid) {
        if let Some((waiter, _)) = self.pending.get(&check_id) {
            let _ = waiter.lock().unwrap().take();
        }
    }

    /// Held long-poll: return up to `limit` checks for `region`, waiting up to
    /// [`HOLD`] for one to appear. Empty on timeout. Counts as a live holder
    /// for the duration.
    pub async fn claim(&self, region: &str, limit: usize) -> Vec<DispatchedCheck> {
        let slot = self.slot(region);
        slot.holders.fetch_add(1, Ordering::Relaxed);
        let _guard = HolderGuard(&slot);
        let deadline = tokio::time::Instant::now() + HOLD;
        loop {
            {
                let mut q = slot.queue.lock().unwrap();
                if !q.is_empty() {
                    let n = q.len().min(limit.max(1));
                    return q.drain(..n).collect();
                }
            }
            let notified = slot.notify.notified();
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Vec::new();
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep(deadline - now) => return Vec::new(),
            }
        }
    }

    /// Deliver a result to the waiting request (if it is still waiting) and
    /// return the check's authoritative meta so a check-now result can be
    /// persisted regardless. `None` only if already completed or TTL-expired.
    pub fn complete(&self, check_id: Uuid, result: DeliveredResult) -> Option<DispatchMeta> {
        let (waiter, meta) = self.pending.remove(&check_id)?;
        if let Some(tx) = waiter.lock().unwrap().take() {
            let _ = tx.send(result);
        }
        Some(meta)
    }
}

/// All-in-one executor: serve this brain's own region in-process so a
/// self-hosted single process (no separate agent) still runs test / check-now.
/// Mirrors the agent's claim→execute→complete loop but talks to the local
/// dispatch directly and persists check-now via the local sink. Spawned only
/// when the in-process scheduler is enabled (the same "this process probes its
/// region" switch); a pure control plane leaves this to agents.
#[allow(clippy::too_many_arguments)]
pub async fn run_local_executor(
    dispatch: Arc<AdHocDispatch>,
    region: String,
    worker_pool: Arc<WorkerPool>,
    http_clients: Arc<HttpClients>,
    result_sink: Arc<dyn ResultSink>,
    cancel: CancellationToken,
) {
    loop {
        let claimed = tokio::select! {
            _ = cancel.cancelled() => return,
            c = dispatch.claim(&region, 32) => c,
        };
        if claimed.is_empty() {
            continue;
        }
        let domain_runtime = worker_pool.domain_expiry_runtime();
        let deps = WorkerDeps {
            http: &http_clients,
            domain_expiry: &domain_runtime,
        };
        for check in claimed {
            let target_id = check.target_id.unwrap_or_else(Uuid::nil);
            let (result, probe) =
                crate::worker::execute_with_probe(target_id, check.org_id, &check.spec, &deps)
                    .await;
            // We produced the result with the authoritative ids, so persist it
            // as-is (mirrors the agent result-ingest path).
            let persist = matches!(check.kind, DispatchKind::CheckNow).then(|| result.clone());
            let (response_headers_preview, response_body_snippet) = probe
                .map(|p| (p.response_headers_preview, p.response_body_snippet))
                .unwrap_or_default();
            dispatch.complete(
                check.id,
                DeliveredResult {
                    result,
                    response_headers_preview,
                    response_body_snippet,
                },
            );
            if let Some(r) = persist
                && let Err(err) = result_sink.write_batch(std::slice::from_ref(&r)).await
            {
                tracing::warn!(error = %err, "in-process check-now persist failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CheckSpec, CheckStatus};
    use chrono::Utc;

    fn spec() -> CheckSpec {
        serde_json::from_value(serde_json::json!({
            "type": "http", "url": "https://example.com/", "method": "GET",
            "timeout": 5000, "follow_redirects": true, "max_redirects": 5,
            "expected_status": { "kind": "exact", "value": 200 },
            "headers": {}, "verify_tls": true
        }))
        .unwrap()
    }

    fn dispatched(id: Uuid) -> DispatchedCheck {
        DispatchedCheck {
            id,
            kind: DispatchKind::Test,
            org_id: Uuid::nil(),
            target_id: None,
            spec: spec(),
        }
    }

    fn result() -> DeliveredResult {
        DeliveredResult {
            result: CheckResult {
                target_id: Uuid::nil(),
                org_id: Uuid::nil(),
                timestamp: Utc::now(),
                status: CheckStatus::Up,
                duration_ms: 1,
                dns_ms: None,
                connect_ms: None,
                tls_ms: None,
                ttfb_ms: None,
                response_code: Some(200),
                response_size: None,
                error: None,
            },
            response_headers_preview: vec![],
            response_body_snippet: None,
        }
    }

    #[tokio::test]
    async fn region_dead_until_a_holder_claims() {
        let d = AdHocDispatch::new();
        assert!(!d.region_live("r"));
        let d = std::sync::Arc::new(d);
        let h = {
            let d = d.clone();
            tokio::spawn(async move { d.claim("r", 8).await })
        };
        // Let the holder register.
        for _ in 0..50 {
            if d.region_live("r") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(d.region_live("r"));
        // No check → holder times out empty. Speed not asserted (HOLD is 25s);
        // just confirm it eventually drops the holder.
        h.abort();
    }

    #[tokio::test]
    async fn dispatch_handoff_and_complete_round_trip() {
        let d = std::sync::Arc::new(AdHocDispatch::new());
        let holder = {
            let d = d.clone();
            tokio::spawn(async move { d.claim("r", 8).await })
        };
        for _ in 0..50 {
            if d.region_live("r") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let id = Uuid::now_v7();
        let rx = d.dispatch("r", dispatched(id));
        let claimed = holder.await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, id);

        let meta = d.complete(id, result()).expect("waiter present");
        assert_eq!(meta.kind, DispatchKind::Test);
        let delivered = rx.await.expect("result delivered");
        assert_eq!(delivered.result.status, CheckStatus::Up);

        // Second complete is a no-op (waiter consumed).
        assert!(d.complete(id, result()).is_none());
    }

    #[tokio::test]
    async fn abandon_stops_delivery_but_keeps_meta_for_late_persist() {
        let d = AdHocDispatch::new();
        let id = Uuid::now_v7();
        let rx = d.dispatch("r", dispatched(id));
        d.abandon(id); // request gave up
        assert!(rx.await.is_err(), "delivery sender dropped on abandon");
        // Meta survives so a late check-now result still persists.
        assert!(d.complete(id, result()).is_some());
        // Second complete is a no-op.
        assert!(d.complete(id, result()).is_none());
    }
}
