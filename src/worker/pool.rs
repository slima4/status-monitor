use std::sync::Arc;

use dashmap::DashMap;
use metrics::{counter, histogram};
use tokio::sync::{Semaphore, mpsc};

use crate::config::CircuitBreakerConfig;
use crate::domain::{CheckResult, CheckSpec, Target};
use crate::http_client::HttpClients;
use crate::observability::metrics::names;
use crate::worker::circuit_breaker::{CIRCUIT_OPEN_REASON, CircuitBreaker};

pub struct CheckTask {
    pub target: Arc<Target>,
}

impl CheckTask {
    pub fn host(&self) -> &str {
        match &self.target.check {
            CheckSpec::Http(http) => http.url.host_str().unwrap_or("unknown"),
            CheckSpec::Tcp(tcp) => &tcp.host,
        }
    }
}

pub struct WorkerPool {
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    http_clients: Arc<HttpClients>,
    breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    breaker_cfg: CircuitBreakerConfig,
    result_tx: mpsc::Sender<CheckResult>,
}

impl WorkerPool {
    pub fn new(
        max_concurrent: usize,
        http_clients: HttpClients,
        breaker_cfg: CircuitBreakerConfig,
        result_tx: mpsc::Sender<CheckResult>,
    ) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            http_clients: Arc::new(http_clients),
            breakers: Arc::new(DashMap::new()),
            breaker_cfg,
            result_tx,
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
        let result_tx = self.result_tx.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let breaker = get_or_init_breaker(&breakers, task.host(), breaker_cfg);

            if !breaker.allow() {
                counter!(names::CHECK_ERRORS, "kind" => "circuit_open").increment(1);
                let result = CheckResult::error(task.target.id, CIRCUIT_OPEN_REASON);
                let _ = result_tx.try_send(result);
                return;
            }

            let result = crate::worker::execute(task.target.id, &task.target.check, &clients).await;
            breaker.record(result.status);
            record_metrics(&result);
            if let Err(err) = result_tx.try_send(result) {
                tracing::warn!(?err, "result channel full or closed");
                counter!(names::STORAGE_DROPPED, "reason" => "queue_full").increment(1);
            }
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
