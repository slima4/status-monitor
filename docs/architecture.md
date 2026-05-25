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
├── api/             REST handlers, router, OpenAPI doc, middleware
│   ├── docs.rs        utoipa OpenApi descriptor (/api/openapi.json + /docs SwaggerUI)
│   ├── error.rs       ApiError envelope + stable error code constants
│   ├── handlers/      one module per resource (targets, results, tags, dashboard, health)
│   ├── idempotency.rs DashMap-backed 24h cache + middleware for bulk + bulk-action
│   ├── middleware.rs  charset=utf-8 rewriter
│   ├── page.rs        PageEnvelope<T> + PageOfTarget / PageOfCheckResult / PageOfIncident / PageOfTagCount
│   ├── redaction.rs   credential redaction wrapper
│   ├── routes.rs      build_router + per-route layer wiring
│   └── types.rs       wire types not in domain/ (TagCount, DashboardSummary, BulkActionRequest, TestRequest, ...)
├── app.rs           AppState (storage + worker pool + caches)
├── bin/loadtest.rs  in-process load test driver
├── config.rs        typed configuration + env override loader
├── domain/          Target, CheckSpec, CheckResult, Incident + coalescing helper
├── error.rs         AppError + IntoResponse → ApiError envelope
├── http_client/     custom hyper-util client + phase-timing connector + hickory resolver
├── observability/   tracing + Prometheus + OTLP setup
├── pipeline/        result batcher
├── scheduler/       target registry + per-target tick loop
├── storage/         Postgres (targets) + ClickHouse (results) + in-memory test doubles
├── web/             askama 0.16 + askama_web HTML routes (dashboard, targets, forms, error pages)
│   ├── routes.rs      Router<AppState> merged into the main router in main.rs
│   ├── assets.rs      rust-embed handler for /static/* with cache-control
│   ├── auth.rs        session cookie scaffolding (v1.1 — no-op today)
│   ├── error.rs       AppError → HTML error page mapper (not the JSON envelope)
│   └── views/         one module per page (dashboard, targets_list, targets_detail, targets_form)
└── worker/          worker pool + circuit breaker + check executors

templates/           askama HTML (compiled into the binary)
└── ... base.html, dashboard{,/region}.html, targets/{list,detail,form}.html, error/{404,500,503}.html

static/              rust-embed bundle
├── css/             Tailwind 4 build output (built by build.rs)
└── js/              HTMX 2 + json-enc + ECharts 6 + tiny UI/chart modules under ui/ and charts/
```

The web layer is a thin server-rendered surface on top of the existing JSON API: every UI mutation hits `/api/v1/*` (forms post JSON, list/detail uses HTMX swaps of partials). See [`ui.md`](ui.md) for operator-level details.

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
                │  ├── http_check (hyper-util + hickory DNS)
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

On-demand checks (`POST /targets/{id}/check-now` and `POST /targets/test`) take a side
path: the handler calls `WorkerPool::run_once` directly. Check-now persists the result
via `ResultSink::write_batch`; test discards it. Both honor the same per-host circuit
breaker as scheduled checks, with `?force=true` available to bypass.

## Key design choices

- **Two storage backends.** Targets are low-cardinality, mutated by API operations → relational (Postgres) is the right fit. Results are append-only, high-cardinality, queried by time range → columnar (ClickHouse) keeps queries fast at 90-day retention.
- **One HTTP client, two TLS modes.** `HttpClients` holds two `hyper_util::client::legacy::Client`s — verifying and insecure — wired around a custom `PhaseConnector` (`src/http_client/connector.rs`). The connector times TCP connect + TLS handshake separately and wraps every connection IO in a `TrackedStream` whose `Drop` decrements `PoolStats.alive`. Per-target `verify_tls` flag picks at dispatch time; both clients share the DNS cache and the same `PoolStats`.
- **Per-host circuit breakers.** Failing hosts open their breaker quickly; subsequent checks fail fast with `error=circuit_open` without consuming a worker slot. Half-open probes after `open_duration_secs`.
- **Per-tenant host throttle (bulkhead).** A fail-fast semaphore caps how many in-flight checks one tenant can run against the same `(host, port)`. Bursts beyond the cap are recorded as `degraded` with `error="throttled: host concurrency cap"` and **do not fire alerts** — the upstream is fine, the back-pressure is operator-side. The cap is keyed per-tenant so one customer's burst can never starve another's monitor of the same host. RDAP carries its own per-TLD cap so one slow registry can't correlate failures across every customer's daily domain-expiry check.
- **Bounded result channel.** The mpsc between worker pool and batcher has a fixed buffer (`storage.clickhouse.buffer_size`). When full, the worker increments `storage_dropped_total{reason="queue_full"}` and drops the result. Back-pressure is explicit, not hidden.
- **Idempotent migrations.** Postgres uses `sqlx::migrate!` (tracked in `_sqlx_migrations`). ClickHouse migrations are bare `CREATE TABLE IF NOT EXISTS` statements run at startup. No external migrator.
- **Shared DNS cache.** A single hickory resolver instance is invoked directly by `PhaseConnector::call`; lookups cache per RFC TTL plus configurable bounds. Per-resolution latency is recorded into `check_dns_ms`.
- **Cancellation tokens for shutdown.** The root token is cloned to scheduler, batcher, sampler, idempotency pruner, and graceful axum shutdown. SIGINT/SIGTERM cancels root; subsystems drain in `tokio::join!`.
- **Self-describing API.** `utoipa` derives an OpenAPI 3.1 document at compile time, exposed at `/api/openapi.json` and rendered at `/docs` via Swagger UI. Every handler annotation carries at least one example. The 4xx/5xx error envelope and the list `PageEnvelope` are unified across every endpoint.
- **In-process caches with bounded TTL.** The dashboard summary holds a 5-second `parking_lot::Mutex<Option<(Instant, DashboardSummary)>>` to absorb operator polling. The `Idempotency-Key` cache is a `DashMap` keyed by `(header, body-hash)` with a 24-hour TTL; a background pruner sweeps expired entries hourly.
- **Incident coalescing.** A shared helper in `domain/incident.rs` consumes ordered `(timestamp, status, error)` tuples and emits `Incident` rows. Memory + ClickHouse storage call into the same logic; the ClickHouse path uses a narrow column projection to keep bandwidth low.

## Concurrency model

- One Tokio runtime, multi-threaded scheduler (default `worker_threads = num_cpus`)
- One Tokio task per active target in the scheduler — sleeps `interval ± jitter`, dispatches, sleeps again
- `WorkerPool::execute` spawns a new task per dispatch, gated by `Arc<Semaphore>` sized to `max_concurrent_checks`
- Batcher is a single task with `tokio::select!` over channel-recv, timeout, and cancellation
- Sampler is a single task that periodically reads gauge sources (pool semaphore counts, target count, breaker counts) and records into the metrics registry
