use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{Semaphore, mpsc};

use crate::config::CircuitBreakerConfig;
use crate::domain::{CheckResult, CheckSpec, Target};
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
    http_client: reqwest::Client,
    breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    breaker_cfg: CircuitBreakerConfig,
    result_tx: mpsc::Sender<CheckResult>,
}

impl WorkerPool {
    pub fn new(
        max_concurrent: usize,
        http_client: reqwest::Client,
        breaker_cfg: CircuitBreakerConfig,
        result_tx: mpsc::Sender<CheckResult>,
    ) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            http_client,
            breakers: Arc::new(DashMap::new()),
            breaker_cfg,
            result_tx,
        }
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
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
                return;
            }
        };

        let client = self.http_client.clone();
        let breakers = self.breakers.clone();
        let breaker_cfg = self.breaker_cfg;
        let result_tx = self.result_tx.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let breaker = get_or_init_breaker(&breakers, task.host(), breaker_cfg);

            if !breaker.allow() {
                let result = CheckResult::error(task.target.id, CIRCUIT_OPEN_REASON);
                let _ = result_tx.try_send(result);
                return;
            }

            let result = crate::worker::execute(task.target.id, &task.target.check, &client).await;
            breaker.record(result.status);
            if let Err(err) = result_tx.try_send(result) {
                tracing::warn!(?err, "result channel full or closed");
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
