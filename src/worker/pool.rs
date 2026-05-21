use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use metrics::{counter, histogram};
use tokio::sync::{Semaphore, mpsc};

use uuid::Uuid;

use crate::config::CircuitBreakerConfig;
use crate::domain::{CheckResult, CheckSpec, OrgId, Target};
use crate::http_client::HttpClients;
use crate::notifier::event::AlertSignal;
use crate::observability::metrics::names;
use crate::worker::circuit_breaker::{BreakerState, CIRCUIT_OPEN_REASON, CircuitBreaker};

pub struct CheckTask {
    pub target: Arc<Target>,
    /// Owning tenant of `target`. Threaded so a check result fans out to the
    /// alert engine with the org needed for tenant-scoped channel resolution.
    pub org_id: OrgId,
}

impl CheckTask {
    pub fn host(&self) -> String {
        host_for_spec(&self.target.check)
    }
}

/// Canonical circuit-breaker key for a CheckSpec. Shared between the scheduler
/// fan-out (CheckTask::host) and the on-demand `check-now` handler so both
/// paths share the same per-host breaker.
pub fn host_for_spec(spec: &CheckSpec) -> String {
    match spec {
        CheckSpec::Http(http) => http.url.host_str().unwrap_or("unknown").to_owned(),
        CheckSpec::Tcp(tcp) => tcp.host.clone(),
        CheckSpec::TlsCert(cert) => cert.host.clone(),
        // Group circuit-breaker state by TLD so a flaky registry doesn't
        // trip the breaker for unrelated TLDs.
        CheckSpec::DomainExpiry(d) => {
            let tld = d.domain.rsplit('.').next().unwrap_or("unknown");
            format!("rdap:{tld}")
        }
        // Custom resolver → key by the resolver itself (one flaky DNS
        // server shouldn't trip the breaker for unrelated targets that
        // happen to share a name); default resolver → key by the queried
        // name so a single broken domain doesn't trip the system breaker.
        CheckSpec::Dns(d) => match &d.resolver {
            Some(addr) => format!("dns:{addr}"),
            None => format!("dns:{}", d.domain),
        },
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
    breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    breaker_cfg: CircuitBreakerConfig,
    fanout: ResultFanout,
}

impl WorkerPool {
    pub fn new(
        max_concurrent: usize,
        http_clients: HttpClients,
        breaker_cfg: CircuitBreakerConfig,
        fanout: ResultFanout,
    ) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            http_clients: Arc::new(http_clients),
            breakers: Arc::new(DashMap::new()),
            breaker_cfg,
            fanout,
        }
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
        let result = crate::worker::execute(target_id, org_id, spec, &self.http_clients).await;
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
        let target = task.target.clone();
        let org_id = task.org_id;

        tokio::spawn(async move {
            let _permit = permit;
            let breaker = get_or_init_breaker(&breakers, &task.host(), breaker_cfg);

            if !breaker.allow() {
                counter!(names::CHECK_ERRORS, "kind" => "circuit_open").increment(1);
                let result = CheckResult::error(task.target.id, org_id.0, CIRCUIT_OPEN_REASON);
                fanout.dispatch(target, org_id, result);
                return;
            }

            let result =
                crate::worker::execute(task.target.id, org_id.0, &task.target.check, &clients)
                    .await;
            breaker.record(result.status);
            record_metrics(&result);
            fanout.dispatch(target, org_id, result);
        });
    }
}

fn get_or_init_breaker(
    breakers: &DashMap<String, Arc<CircuitBreaker>>,
    host: &str,
    cfg: CircuitBreakerConfig,
) -> Arc<CircuitBreaker> {
    if let Some(b) = breakers.get(host) {
        return b.clone();
    }
    breakers
        .entry(host.to_owned())
        .or_insert_with(|| Arc::new(CircuitBreaker::new(cfg)))
        .clone()
}

fn record_metrics(result: &CheckResult) {
    counter!(names::CHECKS_TOTAL, "status" => result.status.as_str()).increment(1);
    histogram!(names::CHECK_DURATION_MS).record(result.duration_ms as f64);
}
