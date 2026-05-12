use std::sync::Arc;
use std::time::{Duration, Instant};

use metrics::{counter, histogram};
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

use crate::domain::CheckResult;
use crate::observability::metrics::names;
use crate::storage::ResultSink;

#[derive(Debug, Clone, Copy)]
pub struct BatcherConfig {
    pub batch_size: usize,
    pub batch_timeout: Duration,
}

pub struct ResultBatcher {
    rx: mpsc::Receiver<CheckResult>,
    sink: Arc<dyn ResultSink>,
    cfg: BatcherConfig,
}

impl ResultBatcher {
    pub fn new(
        rx: mpsc::Receiver<CheckResult>,
        sink: Arc<dyn ResultSink>,
        cfg: BatcherConfig,
    ) -> Self {
        Self { rx, sink, cfg }
    }

    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut buffer: Vec<CheckResult> = Vec::with_capacity(self.cfg.batch_size);
        let mut ticker = interval(self.cfg.batch_timeout);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    drain(&mut self.rx, &mut buffer);
                    if !buffer.is_empty() {
                        flush(&self.sink, &mut buffer).await;
                    }
                    return;
                }
                maybe_result = self.rx.recv() => {
                    let Some(result) = maybe_result else {
                        if !buffer.is_empty() {
                            flush(&self.sink, &mut buffer).await;
                        }
                        return;
                    };
                    buffer.push(result);
                    if buffer.len() >= self.cfg.batch_size {
                        flush(&self.sink, &mut buffer).await;
                    }
                }
                _ = ticker.tick() => {
                    if !buffer.is_empty() {
                        flush(&self.sink, &mut buffer).await;
                    }
                }
            }
        }
    }
}

fn drain(rx: &mut mpsc::Receiver<CheckResult>, buffer: &mut Vec<CheckResult>) {
    while let Ok(r) = rx.try_recv() {
        buffer.push(r);
    }
}

async fn flush(sink: &Arc<dyn ResultSink>, buffer: &mut Vec<CheckResult>) {
    let count = buffer.len();
    histogram!(names::STORAGE_BATCH_SIZE).record(count as f64);
    let start = Instant::now();
    match sink.write_batch(buffer).await {
        Ok(()) => {
            counter!(names::STORAGE_WRITES, "store" => "sink", "result" => "success").increment(1);
        }
        Err(err) => {
            counter!(names::STORAGE_WRITES, "store" => "sink", "result" => "failure").increment(1);
            tracing::error!(?err, count, "batcher flush failed");
        }
    }
    histogram!(names::STORAGE_WRITE_DURATION_MS).record(start.elapsed().as_millis() as f64);
    buffer.clear();
}
