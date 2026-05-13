use std::sync::Arc;

use dashmap::DashMap;
use metrics::{counter, histogram};
use tokio::sync::{Semaphore, mpsc};

use crate::config::CircuitBreakerConfig;
use crate::domain::{CheckResult, CheckSpec, Target};
use crate::http_client::HttpClients;
use crate::notifier::event::AlertSignal;
use crate::observability::metrics::names;
use crate::worker::circuit_breaker::{CIRCUIT_OPEN_REASON, CircuitBreaker};

pub struct CheckTask {
    pub target: Arc<Target>,
}

impl CheckTask {
    pub fn host(&self) -> String {
        match &self.target.check {
            CheckSpec::Http(http) => http.url.host_str().unwrap_or("unknown").to_owned(),
            CheckSpec::Tcp(tcp) => tcp.host.clone(),
            CheckSpec::TlsCert(cert) => cert.host.clone(),
            // Group circuit-breaker state by TLD so a flaky registry doesn't
            // trip the breaker for unrelated TLDs. The "rdap:" prefix keeps
            // the key out of the same namespace as HTTP/TCP hosts (a literal
            // host of "com" would otherwise collide with .com domain checks).
            CheckSpec::DomainExpiry(d) => {
                let tld = d.domain.rsplit('.').next().unwrap_or("unknown");
                format!("rdap:{tld}")
            }
        }
    }
}

/// Fan-out for completed CheckResults: every result goes to the storage mpsc;
/// when alerts are configured a parallel mpsc forwards (target, result) pairs
/// to the alert engine. Both downstreams own independent back-pressure budgets.
#[derive(Clone)]
pub struct ResultFanout {
    storage: mpsc::Sender<CheckResult>,
    alerts: Option<mpsc::Sender<AlertSignal>>,
}

impl ResultFanout {
    pub fn new(
        storage: mpsc::Sender<CheckResult>,
        alerts: Option<mpsc::Sender<AlertSignal>>,
    ) -> Self {
        Self { storage, alerts }
    }

    pub fn storage_only(storage: mpsc::Sender<CheckResult>) -> Self {
        Self::new(storage, None)
    }

    fn dispatch(&self, target: Arc<Target>, result: CheckResult) {
        if let Some(tx) = &self.alerts {
            // Only clone when an alert downstream is attached.
            let signal = AlertSignal {
                target,
                result: result.clone(),
            };
            if tx.try_send(signal).is_err() {
                counter!(names::ALERTS_DROPPED, "reason" => "queue_full").increment(1);
            }
        }
        if let Err(err) = self.storage.try_send(result) {
            tracing::warn!(?err, "result channel full or closed");
            counter!(names::STORAGE_DROPPED, "reason" => "queue_full").increment(1);
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
        use crate::worker::circuit_breaker::BreakerState;
        self.breakers
            .iter()
            .filter(|e| e.value().state() == BreakerState::Open)
            .count()
    }

    pub fn dispatch(&self, task: CheckTask) {
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::debug!(target_id = %task.target.id, "worker pool saturated, dropping task");
                counter!(names::STORAGE_DROPPED, "reason" => "pool_saturated").increment(1);
                return;
            }
        };

        let clients = self.http_clients.clone();
        let breakers = self.breakers.clone();
        let breaker_cfg = self.breaker_cfg;
        let fanout = self.fanout.clone();
        let target = task.target.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let breaker = get_or_init_breaker(&breakers, &task.host(), breaker_cfg);

            if !breaker.allow() {
                counter!(names::CHECK_ERRORS, "kind" => "circuit_open").increment(1);
                let result = CheckResult::error(task.target.id, CIRCUIT_OPEN_REASON);
                fanout.dispatch(target, result);
                return;
            }

            let result = crate::worker::execute(task.target.id, &task.target.check, &clients).await;
            breaker.record(result.status);
            record_metrics(&result);
            fanout.dispatch(target, result);
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
