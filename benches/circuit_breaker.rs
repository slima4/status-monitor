use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use uptimepage::config::CircuitBreakerConfig;
use uptimepage::domain::CheckStatus;
use uptimepage::worker::circuit_breaker::CircuitBreaker;

// Thresholds set high to keep the breaker in the Closed state across the bench,
// isolating allow/record hot-path cost from state-transition cost.
fn cfg() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 1_000_000,
        success_threshold: 1_000_000,
        open_duration_secs: 60,
        half_open_max_calls: 64,
    }
}

fn bench_allow_closed(c: &mut Criterion) {
    let cb = CircuitBreaker::new(cfg());
    c.bench_function("breaker_allow_closed", |b| {
        b.iter(|| black_box(cb.allow()));
    });
}

fn bench_record_success(c: &mut Criterion) {
    let cb = CircuitBreaker::new(cfg());
    c.bench_function("breaker_record_success", |b| {
        b.iter(|| cb.record(black_box(CheckStatus::Up)));
    });
}

fn bench_record_failure(c: &mut Criterion) {
    let cb = CircuitBreaker::new(cfg());
    c.bench_function("breaker_record_failure", |b| {
        b.iter(|| cb.record(black_box(CheckStatus::Down)));
    });
}

criterion_group!(
    benches,
    bench_allow_closed,
    bench_record_success,
    bench_record_failure
);
criterion_main!(benches);
