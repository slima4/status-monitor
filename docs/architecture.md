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
│   ├── idempotency.rs moka 24h cache + middleware for bulk + bulk-action
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

The web layer is a thin server-rendered surface on top of the existing JSON API: every UI mutation hits `/api/v1/*` (forms post JSON, list/detail uses HTMX swaps of partials). See [Development](development.md#web-ui) for the stack and route map, and [Web UI](ui.md) for what the screens do.

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
                │ Scheduler      │  single-driver timing heap, jittered tick
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

On-demand checks (`POST /targets/{id}/check-now` and `POST /targets/test`) are dispatched
to an agent in the target's region over the agent's held long-poll, and the request waits
for the result. The agent persists check-now results (test results are returned but not
stored). If no agent is currently serving the region the request returns `503 PROBE_UNAVAILABLE`.

## Key design choices

- **Two storage backends.** Targets are low-cardinality, mutated by API operations → relational (Postgres) is the right fit. Results are append-only, high-cardinality, queried by time range → columnar (ClickHouse) keeps queries fast at 90-day retention.
- **Fresh-connect HTTP checks, two TLS modes.** `HttpClients` holds two `rustls` `TlsConnector`s — verifying and insecure — plus the shared DNS cache and SSRF guard. There is no connection pool: a monitor probes each target once per interval (a pool rarely reused a socket), and connecting fresh per check is what lets the probe time DNS resolve, TCP connect, and TLS handshake separately (`timed_connect` in `src/http_client/connector.rs`) and write those phases into each result. The request runs over `hyper::client::conn` (h1/h2 by ALPN); the connection task is aborted once the body is read. Per-target `verify_tls` picks the connector at dispatch time.
- **Per-host circuit breakers.** Failing hosts open their breaker quickly; subsequent checks fail fast with `error=circuit_open` without consuming a worker slot. Half-open probes after `open_duration_secs`.
- **Per-tenant host throttle (bulkhead).** A fail-fast semaphore caps how many in-flight checks one tenant can run against the same `(host, port)`. Bursts beyond the cap are recorded as `degraded` with `error="throttled: host concurrency cap"` and **do not fire alerts** — the upstream is fine, the back-pressure is operator-side. The cap is keyed per-tenant so one customer's burst can never starve another's monitor of the same host. RDAP carries its own per-TLD cap so one slow registry can't correlate failures across every customer's daily domain-expiry check.
- **Sticky last-good for domain-expiry probes.** Each successful RDAP probe writes `(expiry_at, registrar, last_success_at)` to `domain_expiry_state` (PK `target_id`, denormalised `org_id`, FK CASCADE on the target). Every trait method requires `OrgId` and the row is filtered by both keys — a handler taking `target_id` from request input cannot read another tenant's row. A subsequent transient failure — RDAP timeout, throttle drop, registry 5xx, 404 — does not flip the monitor: the executor reads the cached row and emits a `CheckResult` with the cached verdict. For Up the `error` field stays empty; for Degraded/Down it carries a `served_stale: …` annotation, so operators can tell the surface from a fresh probe. Cached rows older than 7d (measured against `last_success_at`, never advanced by failures) escalate to `Error`, which is alert-eligible. Cross-tenant singleflight (keyed by canonical domain) collapses concurrent probes for the same domain to one outbound request — RDAP is public registry data, coalescing across tenants is safe and IANA-friendly.
- **Bounded result channel.** The mpsc between worker pool and batcher has a fixed buffer (`storage.clickhouse.buffer_size`). When full, the worker increments `storage_dropped_total{reason="queue_full"}` and drops the result. Back-pressure is explicit, not hidden.
- **Idempotent migrations.** Postgres uses `sqlx::migrate!` (tracked in `_sqlx_migrations`). ClickHouse migrations are bare `CREATE TABLE IF NOT EXISTS` statements run at startup. No external migrator.
- **Shared DNS cache.** A single hickory resolver instance is invoked directly by `timed_connect`; lookups cache per RFC TTL plus configurable bounds. Per-resolution latency is recorded into `check_dns_ms`.
- **Cancellation tokens for shutdown.** The root token is cloned to scheduler, batcher, sampler, and graceful axum shutdown. SIGINT/SIGTERM cancels root; subsystems drain in `tokio::join!`.
- **Self-describing API.** `utoipa` derives an OpenAPI 3.1 document at compile time, exposed at `/api/openapi.json` and rendered at `/docs` via Swagger UI. Every handler annotation carries at least one example. The 4xx/5xx error envelope and the list `PageEnvelope` are unified across every endpoint.
- **In-process caches with bounded TTL.** The dashboard summary holds a 5-second `parking_lot::Mutex<Option<(Instant, DashboardSummary)>>` to absorb operator polling. The `Idempotency-Key` cache is a `moka` cache keyed by `(caller credential, key, body-hash)` with a 24-hour TTL, bounded by a total-bytes weigher.
- **Incident coalescing.** A shared helper in `domain/incident.rs` consumes ordered `(timestamp, status, error)` tuples and emits `Incident` rows. Memory + ClickHouse storage call into the same logic; the ClickHouse path uses a narrow column projection to keep bandwidth low.

## Concurrency model

- One Tokio runtime, multi-threaded scheduler (default `worker_threads = num_cpus`)
- One scheduler driver task owns a min-heap of `(due, target)` keyed by next-due instant — sleeps until the earliest due, dispatches every due target, reschedules at `interval ± jitter`; memory stays flat in the fleet size instead of one task + timer per target
- `WorkerPool::execute` spawns a new task per dispatch, gated by `Arc<Semaphore>` sized to `max_concurrent_checks`
- Batcher is a single task with `tokio::select!` over channel-recv, timeout, and cancellation
- Sampler is a single task that periodically reads gauge sources (pool semaphore counts, target count, breaker counts) and records into the metrics registry

## Multi-region probes

By default one process is the whole system: it schedules and runs every check itself, in one region. A deployment can add regions by running extra processes as **agents** (`[agent] enabled = true`) — stateless probes with no database, web, or alerting. Each agent pulls its region's decrypted monitor config from the control plane and POSTs results back; region is the partition key, so one agent per region needs no coordination. The control plane's own region is a normal region row (`scheduler.region`), not a sentinel. Results carry their region + agent through both ClickHouse rollups, so reads can slice by region. Regions and agents are provisioned through the instance-admin `/operator/*` surface. See [Multi-region probes](multi-region.md) for the full model, operator surface, and read-path behaviour.
