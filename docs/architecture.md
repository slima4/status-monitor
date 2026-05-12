# Architecture

## Goals

- Run periodic HTTP + TCP health checks against an arbitrary, mutable set of targets
- Stay below 50 ms p99 overhead per check (excluding network)
- Sustain ~50k concurrent in-flight checks per node
- Survive transient target failures (per-host circuit breakers) and storage flaps (in-process retry + batching)
- Graceful shutdown within 10 s without losing in-flight results

## Module layout

```
src/
├── api/             REST handlers and router
├── app.rs           AppState
├── bin/loadtest.rs  in-process load test driver
├── config.rs        typed configuration + env override loader
├── domain/          Target, CheckSpec, CheckResult and friends
├── error.rs         AppError + IntoResponse
├── http_client/     tuned reqwest client + hickory resolver
├── observability/   tracing + Prometheus + OTLP setup
├── pipeline/        result batcher
├── scheduler/       target registry + per-target tick loop
├── storage/         Postgres (targets) + ClickHouse (results) + in-memory test doubles
└── worker/          worker pool + circuit breaker + check executors
```

## Data flow

```
                ┌────────────────┐
                │ REST API       │  target CRUD
                │ (axum + AppState)
                └────────┬───────┘
                         │ writes
                         ▼
                ┌────────────────┐
                │ PostgreSQL     │  target metadata
                └────────┬───────┘
                         │ TargetRegistry.refresh() every N seconds
                         ▼
                ┌────────────────┐
                │ Scheduler      │  one task per target, jittered tick
                └────────┬───────┘
                         │ dispatch
                         ▼
                ┌────────────────┐
                │ WorkerPool     │  semaphore-bounded, circuit-breaker-gated
                │  ├── http_check (reqwest + hickory DNS)
                │  └── tcp_check  (tokio::net::TcpStream)
                └────────┬───────┘
                         │ CheckResult on mpsc channel
                         ▼
                ┌────────────────┐
                │ ResultBatcher  │  size + timeout flush
                └────────┬───────┘
                         │ write_batch
                         ▼
                ┌────────────────┐
                │ ClickHouse     │  check_results + 1-min agg MV
                └────────────────┘
```

## Key design choices

- **Two storage backends.** Targets are low-cardinality, mutated by API operations → relational (Postgres) is the right fit. Results are append-only, high-cardinality, queried by time range → columnar (ClickHouse) keeps queries fast at 90-day retention.
- **One HTTP client, two TLS modes.** `HttpClients` holds a verifying and an insecure reqwest client sharing a single DNS cache. Per-target `verify_tls` flag picks at dispatch time. Avoids per-target client construction.
- **Per-host circuit breakers.** Failing hosts open their breaker quickly; subsequent checks fail fast with `error=circuit_open` without consuming a worker slot. Half-open probes after `open_duration_secs`.
- **Bounded result channel.** The mpsc between worker pool and batcher has a fixed buffer (`storage.clickhouse.buffer_size`). When full, the worker increments `storage_dropped_total{reason="queue_full"}` and drops the result. Back-pressure is explicit, not hidden.
- **Idempotent migrations.** Postgres uses `sqlx::migrate!` (tracked in `_sqlx_migrations`). ClickHouse migrations are bare `CREATE TABLE IF NOT EXISTS` statements run at startup. No external migrator.
- **Shared DNS cache.** A single hickory resolver instance serves both reqwest clients; lookups cache per RFC TTL plus configurable bounds. Per-resolution latency is recorded into `check_dns_ms`.
- **Cancellation tokens for shutdown.** The root token is cloned to scheduler, batcher, sampler, and graceful axum shutdown. SIGINT/SIGTERM cancels root; subsystems drain in `tokio::join!`.

## Concurrency model

- One Tokio runtime, multi-threaded scheduler (default `worker_threads = num_cpus`)
- One Tokio task per active target in the scheduler — sleeps `interval ± jitter`, dispatches, sleeps again
- `WorkerPool::execute` spawns a new task per dispatch, gated by `Arc<Semaphore>` sized to `max_concurrent_checks`
- Batcher is a single task with `tokio::select!` over channel-recv, timeout, and cancellation
- Sampler is a single task that periodically reads gauge sources (pool semaphore counts, target count, breaker counts) and records into the metrics registry
