use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use metrics::{Counter, counter, histogram};
use tokio::sync::{Semaphore, mpsc};

use uuid::Uuid;

use crate::config::CircuitBreakerConfig;
use crate::domain::{CheckResult, CheckSpec, CheckStatus, HOST_THROTTLE_REASON, OrgId, Target};
use crate::http_client::HttpClients;
use crate::notifier::event::AlertSignal;
use crate::observability::metrics::names;
use crate::worker::circuit_breaker::{BreakerState, CIRCUIT_OPEN_REASON, CircuitBreaker};
use crate::worker::host_throttle::{HostPermit, HostThrottle, Throttled};

// Hot-path counters resolved once. `counter!` rebuilds the label set on
// every call — at high QPS that's a per-check allocation we don't need.
// `host_throttle_waits_total{kind=rdap}` is incremented by the
// domain_expiry executor since RDAP no longer pre-throttles at the pool
// level.
static HOST_THROTTLE_WAITS_HOST: LazyLock<Counter> =
    LazyLock::new(|| counter!(names::HOST_THROTTLE_WAITS, "kind" => "host"));
static HOST_THROTTLE_DROPS_C: LazyLock<Counter> =
    LazyLock::new(|| counter!(names::HOST_THROTTLE_DROPS));

pub struct CheckTask {
    pub target: Arc<Target>,
    /// Owning tenant of `target`. Threaded so a check result fans out to the
    /// alert engine with the org needed for tenant-scoped channel resolution.
    pub org_id: OrgId,
    /// Pre-computed throttle key from `ScheduledTarget::build`. `None` for
    /// check kinds the throttle skips (DNS, DomainExpiry — RDAP path).
    pub host_key: Option<crate::worker::host_throttle::HostKey>,
    /// Pre-computed circuit-breaker key. Cheap Arc clone — never recomputed
    /// on the dispatch hot path.
    pub breaker_key: Arc<str>,
    /// Pre-computed RDAP TLD for DomainExpiry checks.
    pub rdap_tld: Option<Arc<str>>,
}

impl CheckTask {
    pub fn breaker_key(&self) -> &str {
        &self.breaker_key
    }
}

/// Canonical circuit-breaker key for a CheckSpec. Shared between the scheduler
/// fan-out and the on-demand `check-now` handler so both paths share the
/// same per-host breaker. Hosts are normalized (lowercased + trailing-dot
/// stripped) — otherwise `Example.com` and `example.com` from two targets
/// allocate two breakers + grow the map without bound under tenant-
/// controlled casing.
pub fn host_for_spec(spec: &CheckSpec) -> String {
    if let Some((host, _)) = crate::worker::host_throttle::host_port_raw(spec) {
        return crate::worker::host_throttle::canonical_host(host);
    }
    match spec {
        // Group circuit-breaker state by TLD so a flaky registry doesn't
        // trip the breaker for unrelated TLDs.
        CheckSpec::DomainExpiry(d) => {
            let tld = d.domain.rsplit('.').next().unwrap_or("unknown");
            format!("rdap:{}", tld.to_ascii_lowercase())
        }
        // Custom resolver → key by the resolver itself; default → key by
        // the queried name so a single broken domain doesn't trip the
        // system breaker.
        CheckSpec::Dns(d) => match &d.resolver {
            Some(addr) => format!("dns:{addr}"),
            None => format!("dns:{}", d.domain.to_ascii_lowercase()),
        },
        // host_port_raw already covered these; reaching here means an
        // HTTP URL with no host_str. Listed explicitly so a future
        // CheckSpec variant is forced through the match and can't
        // silently collapse onto this catch-all.
        CheckSpec::Http(_) | CheckSpec::Tcp(_) | CheckSpec::TlsCert(_) => "unknown".to_owned(),
    }
}

/// Fan-out for completed CheckResults: every result goes to the storage mpsc;
/// when alerts are configured a parallel mpsc forwards (target, result) pairs
/// to the alert engine. Both downstreams own independent back-pressure budgets.
#[derive(Clone)]
pub struct ResultFanout {
    storage: mpsc::Sender<CheckResult>,
    alerts: Option<mpsc::Sender<AlertSignal>>,
    storage_dropped: Arc<AtomicU64>,
}

impl ResultFanout {
    pub fn new(
        storage: mpsc::Sender<CheckResult>,
        alerts: Option<mpsc::Sender<AlertSignal>>,
    ) -> Self {
        Self {
            storage,
            alerts,
            storage_dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn storage_only(storage: mpsc::Sender<CheckResult>) -> Self {
        Self::new(storage, None)
    }

    pub fn queue_depth(&self) -> u64 {
        let max = self.storage.max_capacity();
        max.saturating_sub(self.storage.capacity()) as u64
    }

    pub fn dropped(&self) -> u64 {
        self.storage_dropped.load(Ordering::Relaxed)
    }

    pub fn note_storage_dropped(&self) {
        self.storage_dropped.fetch_add(1, Ordering::Relaxed);
    }

    fn dispatch(&self, target: Arc<Target>, org_id: OrgId, result: CheckResult) {
        if let Some(tx) = &self.alerts {
            // Only clone when an alert downstream is attached.
            let signal = AlertSignal {
                target,
                org_id,
                result: result.clone(),
            };
            if tx.try_send(signal).is_err() {
                counter!(names::ALERTS_DROPPED, "reason" => "queue_full").increment(1);
            }
        }
        self.dispatch_storage(result);
    }

    /// Storage-only dispatch — used for results that must not trigger alert
    /// fan-out (e.g. throttle drops where the upstream is fine and the
    /// "Degraded" status is operator-side back-pressure, not a real fault).
    fn dispatch_storage_only(&self, result: CheckResult) {
        self.dispatch_storage(result);
    }

    fn dispatch_storage(&self, result: CheckResult) {
        if let Err(err) = self.storage.try_send(result) {
            tracing::warn!(?err, "result channel full or closed");
            counter!(names::STORAGE_DROPPED, "reason" => "queue_full").increment(1);
            self.note_storage_dropped();
        }
    }
}

pub struct WorkerPool {
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    http_clients: Arc<HttpClients>,
    breakers: Arc<DashMap<Arc<str>, Arc<CircuitBreaker>>>,
    breaker_cfg: CircuitBreakerConfig,
    fanout: ResultFanout,
    host_throttle: Arc<HostThrottle>,
    domain_expiry: Arc<crate::worker::domain_expiry::DomainExpiryRuntime>,
}

impl WorkerPool {
    pub fn new(
        max_concurrent: usize,
        http_clients: HttpClients,
        breaker_cfg: CircuitBreakerConfig,
        fanout: ResultFanout,
        host_throttle: Arc<HostThrottle>,
        domain_expiry: Arc<crate::worker::domain_expiry::DomainExpiryRuntime>,
    ) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            http_clients: Arc::new(http_clients),
            breakers: Arc::new(DashMap::new()),
            breaker_cfg,
            fanout,
            host_throttle,
            domain_expiry,
        }
    }

    pub fn host_throttle(&self) -> Arc<HostThrottle> {
        self.host_throttle.clone()
    }

    pub fn domain_expiry_runtime(&self) -> Arc<crate::worker::domain_expiry::DomainExpiryRuntime> {
        self.domain_expiry.clone()
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    pub fn in_flight(&self) -> usize {
        self.max_concurrent
            .saturating_sub(self.semaphore.available_permits())
    }

    pub fn open_breakers(&self) -> usize {
        self.breakers
            .iter()
            .filter(|e| e.value().state() == BreakerState::Open)
            .count()
    }

    pub fn result_queue_depth(&self) -> u64 {
        self.fanout.queue_depth()
    }

    pub fn dropped_results(&self) -> u64 {
        self.fanout.dropped()
    }

    pub fn http_clients(&self) -> Arc<HttpClients> {
        self.http_clients.clone()
    }

    pub fn breaker_for(&self, host: &str) -> Arc<CircuitBreaker> {
        get_or_init_breaker(&self.breakers, host, self.breaker_cfg)
    }

    /// Drop breakers whose only strong reference is the map and that are
    /// in the steady `Closed` state. Run from the scheduler tick to bound
    /// the breaker map under target churn (user-driven host edits, deletes).
    pub fn sweep_breakers(&self) -> usize {
        let stale: Vec<Arc<str>> = self
            .breakers
            .iter()
            .filter(|e| {
                Arc::strong_count(e.value()) == 1 && e.value().state() == BreakerState::Closed
            })
            .map(|e| e.key().clone())
            .collect();
        let mut removed = 0usize;
        for k in stale {
            if self
                .breakers
                .remove_if(&k, |_, v| {
                    Arc::strong_count(v) == 1 && v.state() == BreakerState::Closed
                })
                .is_some()
            {
                removed += 1;
            }
        }
        removed
    }

    /// Runs a one-off check against `target` honoring the per-host circuit
    /// breaker. Returns `None` if the breaker is open and `force` is false.
    /// Result is recorded on the breaker but NOT dispatched through the
    /// pool's fanout — caller decides whether to persist.
    pub async fn run_once(
        &self,
        target_id: Uuid,
        org_id: Uuid,
        spec: &CheckSpec,
        host: &str,
        force: bool,
    ) -> Option<CheckResult> {
        let breaker = self.breaker_for(host);
        if !force && breaker.state() == BreakerState::Open && !breaker.allow() {
            return None;
        }
        let deps = crate::worker::WorkerDeps {
            http: &self.http_clients,
            domain_expiry: &self.domain_expiry,
        };
        let result = crate::worker::execute(target_id, org_id, spec, &deps).await;
        breaker.record(result.status);
        Some(result)
    }

    pub fn dispatch(&self, task: CheckTask) {
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::debug!(target_id = %task.target.id, "worker pool saturated, dropping task");
                counter!(names::STORAGE_DROPPED, "reason" => "pool_saturated").increment(1);
                self.fanout.note_storage_dropped();
                return;
            }
        };

        let clients = self.http_clients.clone();
        let breakers = self.breakers.clone();
        let breaker_cfg = self.breaker_cfg;
        let fanout = self.fanout.clone();
        let throttle = self.host_throttle.clone();
        let domain_expiry = self.domain_expiry.clone();
        let target = task.target.clone();
        let org_id = task.org_id;

        tokio::spawn(async move {
            let _permit = permit;
            let breaker = get_or_init_breaker(&breakers, &task.breaker_key, breaker_cfg);

            if !breaker.allow() {
                counter!(names::CHECK_ERRORS, "kind" => "circuit_open").increment(1);
                let result = CheckResult::error(task.target.id, org_id.0, CIRCUIT_OPEN_REASON);
                fanout.dispatch(target, org_id, result);
                return;
            }

            let host_permit = match acquire_host_slot(&throttle, &task) {
                Ok(p) => p,
                Err(Throttled) => {
                    HOST_THROTTLE_DROPS_C.increment(1);
                    let mut result =
                        CheckResult::error(task.target.id, org_id.0, HOST_THROTTLE_REASON);
                    result.status = CheckStatus::Degraded;
                    fanout.dispatch_storage_only(result);
                    return;
                }
            };

            let deps = crate::worker::WorkerDeps {
                http: &clients,
                domain_expiry: &domain_expiry,
            };
            let result =
                crate::worker::execute(task.target.id, org_id.0, &task.target.check, &deps).await;
            breaker.record(result.status);
            record_metrics(&result);
            drop(host_permit);
            fanout.dispatch(target, org_id, result);
        });
    }
}

fn acquire_host_slot(
    throttle: &HostThrottle,
    task: &CheckTask,
) -> Result<Option<HostPermit>, Throttled> {
    // RDAP/DomainExpiry no longer pre-throttles here — the executor owns
    // that path so it can fall back to the sticky last-good cache instead
    // of letting the pool synthesize a Degraded row. `rdap_tld` is still
    // pre-computed on `ScheduledTarget` for the executor's use.
    if task.rdap_tld.is_some() {
        return Ok(None);
    }
    if let Some(key) = task.host_key.as_ref() {
        HOST_THROTTLE_WAITS_HOST.increment(1);
        return throttle.acquire(key).map(Some);
    }
    Ok(None)
}

fn get_or_init_breaker(
    breakers: &DashMap<Arc<str>, Arc<CircuitBreaker>>,
    host: &str,
    cfg: CircuitBreakerConfig,
) -> Arc<CircuitBreaker> {
    if let Some(b) = breakers.get(host) {
        return b.clone();
    }
    breakers
        .entry(Arc::from(host))
        .or_insert_with(|| Arc::new(CircuitBreaker::new(cfg)))
        .clone()
}

fn record_metrics(result: &CheckResult) {
    counter!(names::CHECKS_TOTAL, "status" => result.status.as_str()).increment(1);
    histogram!(names::CHECK_DURATION_MS).record(result.duration_ms as f64);
}
