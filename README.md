# status-monitor

Async Rust service that runs HTTP and TCP health checks against a configurable set of targets, applies per-host circuit breaking, batches results, and ships them to a result sink. Backends for ClickHouse (results) and PostgreSQL (target metadata) are included under `src/storage/`; the default binary wires in-memory stores so it runs without external dependencies. Exposes a REST API for target CRUD and result queries, plus Prometheus metrics on a separate port.

## Quick start

### Local build

```bash
cargo build --release
./target/release/status-monitor
```

The service reads `config/default.toml` at startup. Every key can be overridden by an environment variable using the prefix `STATUS_MONITOR_` and `__` as the nested separator. Example: `STATUS_MONITOR_SERVER__API_BIND=0.0.0.0:8080`.

### Docker

```bash
docker compose up --build
```

This brings up Postgres 17, ClickHouse 25, and the monitor itself. The Postgres and ClickHouse schemas under `migrations/` are auto-loaded on first start.

The runtime image is built on top of `gcr.io/distroless/cc-debian12:nonroot` to keep the attack surface small.

## Configuration

Defaults live in `config/default.toml`. The most useful knobs:

| Section | Key | Purpose |
|---------|-----|---------|
| `server` | `api_bind`, `metrics_bind` | bind addresses for the REST API and the Prometheus exporter |
| `checker` | `max_concurrent_checks` | global concurrency cap enforced by the worker pool semaphore |
| `checker` | `default_timeout_ms`, `connect_timeout_ms` | client-side timeouts applied to outbound checks |
| `http_client` | pool / keep-alive settings | reqwest tuning forwarded to the shared client |
| `dns` | `cache_size`, `servers` | hickory resolver — point at internal resolvers when needed |
| `circuit_breaker` | thresholds + cooldown | per-host breaker state machine |
| `storage.postgres` | `url`, pool sizing | target metadata store |
| `storage.clickhouse` | `url`, batch sizing, buffer | result sink and pipeline back-pressure |
| `scheduler` | `target_refresh_interval_secs`, `jitter_pct` | how often the registry is reconciled against Postgres, and how much jitter is applied to each target's tick |
| `observability` | `log_level`, `log_format`, `metrics_enabled` | tracing-subscriber configuration |

Override `STATUS_MONITOR_CONFIG_PATH` to point at an alternate base config file.

## REST API

Mounted under `/api/v1` on the API port. JSON in, JSON out. No authentication in v1 — bind to loopback or front it with a reverse proxy you trust.

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/v1/targets` | create one target |
| `POST` | `/api/v1/targets/bulk` | bulk-create up to 10 000 targets |
| `GET` | `/api/v1/targets` | list targets (`limit`, `offset`, `tag`, `enabled` query params) |
| `GET` | `/api/v1/targets/{id}` | get one target |
| `PATCH` | `/api/v1/targets/{id}` | update name, check spec, interval, enabled, tags |
| `DELETE` | `/api/v1/targets/{id}` | delete a target |
| `GET` | `/api/v1/targets/{id}/results` | recent check results (`from`, `to`, `limit`) |
| `GET` | `/api/v1/targets/{id}/uptime` | uptime summary over a range |
| `GET` | `/healthz` | liveness — always 200 once the process is up |
| `GET` | `/readyz` | readiness — pings the target store; 503 if unreachable |

Check specs are tagged enums:

```jsonc
{
  "type": "http",
  "url": "https://example.com/healthz",
  "method": "GET",
  "timeout": 5000,
  "follow_redirects": false,
  "max_redirects": 0,
  "expected_status": { "kind": "exact", "value": 200 },
  "headers": {},
  "verify_tls": true
}
```

```jsonc
{ "type": "tcp", "host": "db.internal", "port": 5432, "timeout": 2000 }
```

## Metrics

Prometheus exposition on `metrics_bind` (default `127.0.0.1:9090/metrics`). Notable series:

- `status_monitor_checks_total{status}` — checks completed, partitioned by terminal status
- `status_monitor_check_errors_total{kind}` — error breakdown (timeout / connect / circuit_open / …)
- `status_monitor_check_duration_ms` — histogram of per-check wall time
- `status_monitor_storage_writes_total{store,result}` — batcher flush outcomes
- `status_monitor_storage_batch_size` — histogram of flush sizes
- `status_monitor_storage_write_duration_ms` — histogram of flush durations
- `status_monitor_storage_dropped_total{reason}` — checks dropped before reaching the sink
- `status_monitor_build_info{version}` — incremented once at startup so the endpoint is never empty

Scrape interval of 15 s is plenty — most counters are written from hot tokio tasks.

## Deployment

- **Bind addresses:** defaults are loopback. Override via env in production. There is no built-in auth on the API port; front it with a proxy or keep it on a private network.
- **Migrations:** SQL under `migrations/postgres/` and `migrations/clickhouse/` is plain numbered SQL. Apply with your migration tool of choice. The compose file mounts them as Docker init scripts, which is fine for first boot but not for repeated upgrades.
- **Resource sizing:** `max_concurrent_checks` is the global limit on outbound checks in flight. The pool spawns one tokio task per dispatch and gates entry behind a semaphore, so memory scales with that ceiling.
- **Default storage:** the binary ships with the in-memory target store and result sink wired in `src/main.rs`. The Postgres and ClickHouse implementations exist under `src/storage/` and can be swapped in; config-driven backend selection is roadmap work.
- **Shutdown:** the binary listens for SIGINT and SIGTERM, cancels the scheduler and batcher via a shared `CancellationToken`, awaits both background tasks, and exits within 10 s. The batcher's cancel branch drains any pending results before returning. A warning is logged if the deadline is exceeded.

## Load testing

```bash
cargo run --release --bin loadtest -- # env knobs documented below
```

Tunable via env:

- `CONCURRENCY` (default `50000`) — number of concurrent virtual workers
- `DURATION_SECS` (default `30`) — how long to drive load
- `TIMEOUT_MS` (default `5000`) — per-check timeout

The harness spins an in-process mock axum server returning `200 ok`, then drives the configured number of workers in a tight loop against it using the same `build_client` + check executor the production binary uses. It prints `total`, `rps`, and `p50/p95/p99` latency at the end.

## Benchmarks

```bash
cargo bench
```

Two suites:

- `circuit_breaker` — `allow` / `record` micro-benchmarks
- `batcher_flush` — `InMemorySink::write_batch` at 100 / 1 000 / 10 000 element batch sizes

## Troubleshooting

- **`/readyz` returns 503:** the target store can't be reached. Check the `storage.postgres.url` and that Postgres is up.
- **No metrics on `/metrics`:** confirm `observability.metrics_enabled = true` and that `metrics_bind` isn't being blocked by a local firewall. A `status_monitor_build_info` gauge is emitted at startup so the endpoint is never truly empty.
- **Many `storage_dropped_total{reason="queue_full"}`:** the result channel is back-pressured. Raise `storage.clickhouse.buffer_size`, increase `batch_size`, or lower `default_check_interval_secs` for the busiest targets.
- **Circuit breaker stuck open:** look at `status_monitor_check_errors_total{kind}` per host to find the failure mode, then wait `circuit_breaker.open_duration_secs` for the breaker to enter half-open and probe.
- **TLS errors against internal hosts:** the global client always verifies TLS in v1; `verify_tls` per target is accepted by the API but not enforced yet.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
```

Layout:

```
src/
├── api/          REST handlers and router
├── app.rs        AppState
├── config.rs     typed configuration + env override loader
├── domain/       Target, CheckSpec, CheckResult and friends
├── error.rs      AppError + IntoResponse
├── http_client/  tuned reqwest client + hickory resolver
├── observability/ tracing + metrics setup
├── pipeline/     result batcher
├── scheduler/    target registry + per-target tick loop
├── storage/      Postgres + ClickHouse + in-memory backends
└── worker/       worker pool + circuit breaker + check executors
```
