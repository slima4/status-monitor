# status-monitor

[![docs](https://github.com/slima4/status-monitor/actions/workflows/docs.yml/badge.svg)](https://github.com/slima4/status-monitor/actions/workflows/docs.yml)

Async Rust service that runs HTTP and TCP health checks against a configurable set of targets, applies per-host circuit breaking, batches results, and ships them to durable storage. Targets persist in PostgreSQL; check results land in ClickHouse for high-cardinality time-series queries. Exposes a REST API for target CRUD and result queries plus Prometheus metrics on a separate port.

Built on Rust 1.95 (edition 2024), Tokio, Axum, reqwest, sqlx, and the official `clickhouse` crate. Designed for low-overhead checks at ~50k concurrent in-flight.

**Full docs: <https://slima4.github.io/status-monitor/>**

## Quick start

### Docker (recommended)

```bash
docker compose up -d
```

Brings up Postgres 17, ClickHouse 25.8, and the monitor. Migrations for both databases run at process startup — no init-script wiring, no external migrator.

Create a target:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/targets \
  -H 'content-type: application/json' \
  -d '{
    "name": "example",
    "check": {
      "type": "http",
      "url": "https://example.com/",
      "method": "GET",
      "timeout": 5000,
      "follow_redirects": false,
      "max_redirects": 0,
      "expected_status": { "kind": "exact", "value": 200 },
      "headers": {},
      "verify_tls": true
    },
    "interval": 30,
    "enabled": true,
    "tags": []
  }'
```

Read uptime:

```bash
curl http://127.0.0.1:8080/api/v1/targets/<id>/uptime
```

Scrape metrics:

```bash
curl http://127.0.0.1:9090/metrics
```

### Local build

```bash
cargo build --release
./target/release/status-monitor
```

Requires Postgres and ClickHouse reachable at the URLs in `config/default.toml`. To run against the compose stack without rebuilding the container:

```bash
docker compose up -d postgres clickhouse
cargo run --release
```

## Docs

Hosted: <https://slima4.github.io/status-monitor/>

Sources under [`docs/`](docs/) — readable directly on GitHub too:

| File | Covers |
|---|---|
| [docs/architecture.md](docs/architecture.md) | goals, module layout, data flow, key design choices, concurrency model |
| [docs/api.md](docs/api.md) | REST endpoints, check-spec payload shapes, result + uptime queries |
| [docs/configuration.md](docs/configuration.md) | `default.toml` reference, env override scheme, tuning notes |
| [docs/metrics.md](docs/metrics.md) | Prometheus series, OpenTelemetry tracing, planned-but-not-emitted series |
| [docs/deployment.md](docs/deployment.md) | Docker, bind addresses, migrations, sizing, graceful shutdown |
| [docs/loadtest.md](docs/loadtest.md) | `bin/loadtest` envs, macOS gotchas, HTTP/1 vs h2c trade-off, Linux container path |
| [docs/benchmarks.md](docs/benchmarks.md) | Criterion micro-benchmarks, single-core throughput, profile breakdown |
| [docs/troubleshooting.md](docs/troubleshooting.md) | common failures and how to read them off metrics |

## Development

Requires Rust 1.95+ (edition 2024). Install via `rustup`.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
cargo bench
```

Postgres-backed tests (e.g. `bulk_create_with_ragged_tags`) are `#[ignore]`'d by default. Bring up the compose stack and opt in:

```bash
docker compose up -d postgres
DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/monitor \
  cargo test -- --ignored
```
