use chrono::Utc;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use status_monitor::domain::{CheckResult, CheckStatus};
use status_monitor::storage::{InMemorySink, ResultSink};
use std::hint::black_box;
use std::sync::Arc;
use tokio::runtime::Builder;
use uuid::Uuid;

fn sample_results(n: usize) -> Vec<CheckResult> {
    let target = Uuid::now_v7();
    let now = Utc::now();
    (0..n)
        .map(|i| CheckResult {
            target_id: target,
            timestamp: now,
            status: CheckStatus::Up,
            duration_ms: 25 + (i as u32 % 50),
            dns_ms: Some(1),
            connect_ms: Some(2),
            tls_ms: Some(3),
            ttfb_ms: Some(10),
            response_code: Some(200),
            response_size: Some(512),
            error: None,
        })
        .collect()
}

fn bench_inmem_write(c: &mut Criterion) {
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    let mut group = c.benchmark_group("batcher_flush_inmem");
    for &size in &[100usize, 1_000, 10_000] {
        let results = sample_results(size);
        group.throughput(criterion::Throughput::Elements(size as u64));
        group.bench_function(format!("size_{size}"), |b| {
            b.to_async(&rt).iter_batched(
                || {
                    let sink: Arc<dyn ResultSink> = Arc::new(InMemorySink::new());
                    sink
                },
                |sink: Arc<dyn ResultSink>| {
                    let results = &results;
                    async move {
                        sink.write_batch(black_box(results)).await.unwrap();
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_inmem_write);
criterion_main!(benches);
