# Troubleshooting

## `/readyz` returns 503

The target store can't be reached. Check `storage.postgres.url` and that Postgres is up. The readiness probe pings the store; liveness (`/healthz`) does not.

## No metrics on `/metrics`

- Confirm `observability.metrics_enabled = true`
- Confirm `metrics_bind` isn't blocked by a local firewall
- `status_monitor_build_info` is emitted at startup so the endpoint is never truly empty — if it's also missing, the metrics exporter never bound

## Many `storage_dropped_total{reason="queue_full"}`

The result channel between worker pool and batcher is back-pressured.

- Raise `storage.clickhouse.buffer_size` (mpsc capacity)
- Raise `storage.clickhouse.batch_size` (fewer round-trips per batch)
- Lower `storage.clickhouse.batch_timeout_ms` (more frequent flushes)
- Or lower check frequency for the busiest targets (`interval` per target)

## Circuit breaker stuck open

Look at `status_monitor_check_errors_total{kind}` filtered by host to find the failure mode, then wait `circuit_breaker.open_duration_secs` for the breaker to enter half-open and probe.

## TLS errors against internal hosts

Set `verify_tls: false` on the offending target. The check executor picks between a verifying and a non-verifying hyper-util client based on the flag — both share the same DNS cache and connection-pool sizing.

## ClickHouse insert fails with `SchemaMismatch`

Almost always a Row-derive mismatch on UUID, Enum8, or DateTime64 column types:

- UUID columns require `#[serde(with = "clickhouse::serde::uuid")]` on the field
- Enum8 columns require an `i8` field, not `&str`
- DateTime64 filter binds in `WHERE` clauses need wrapping in `fromUnixTimestamp64Milli(?)` — raw `i64` won't coerce to DateTime64 in CH expressions

## Loadtest reports `connect` errors at high concurrency

Loopback ephemeral port exhaustion or kernel SYN backlog overflow. See [loadtest.md](loadtest.md) — set `MOCK_PORTS=64`, `RAMP_SECS=30`, or enable `HTTP2=1`.
