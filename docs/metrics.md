# Metrics

Prometheus exposition on `metrics_bind` (default `127.0.0.1:9090/metrics`).

## Series

| Name | Type | Purpose |
|---|---|---|
| `status_monitor_checks_total{status}` | counter | checks completed, partitioned by terminal status (`up`/`down`/`degraded`/`error`) |
| `status_monitor_check_errors_total{kind}` | counter | error breakdown (`timeout` / `connect` / `circuit_open` / `request` / `body` / `transport`) |
| `status_monitor_check_duration_ms` | histogram | per-check wall time |
| `status_monitor_check_dns_ms` | histogram | DNS resolution latency (recorded in the hickory wrapper) |
| `status_monitor_check_ttfb_ms` | histogram | time-to-first-byte across the response start |
| `status_monitor_storage_writes_total{store,result}` | counter | batcher flush outcomes |
| `status_monitor_storage_batch_size` | histogram | flush batch sizes |
| `status_monitor_storage_write_duration_ms` | histogram | flush durations |
| `status_monitor_storage_dropped_total{reason}` | counter | checks dropped before reaching the sink (queue full, etc.) |
| `status_monitor_circuit_breaker_transitions_total{from,to}` | counter | breaker state changes |
| `status_monitor_open_breakers` | gauge | currently-open breakers (sampled) |
| `status_monitor_active_targets` | gauge | enabled targets known to the registry (sampled) |
| `status_monitor_inflight_checks` | gauge | current worker-pool semaphore depth (sampled) |
| `status_monitor_build_info{version}` | gauge | incremented once at startup so the endpoint is never empty |

Scrape interval of 15 s is plenty — counters are written from hot tokio tasks; histograms aggregate per bucket without lock contention.

## OpenTelemetry tracing

`observability.tracing_enabled = true` ships spans to `otlp_endpoint` (OTLP/gRPC, default `http://localhost:4317`). Compatible with any modern OTel collector — Tempo, Jaeger, Datadog Agent, Honeycomb, etc.

Spans are emitted around:

- HTTP / TCP check execution
- ClickHouse batch flush
- Postgres target store ops

## Not yet emitted (planned)

The following SPEC §6.11 metrics need a custom hyper connector to expose; current `reqwest` 0.13 has no public hook for them:

- `status_monitor_check_connect_ms` — TCP connect latency
- `status_monitor_check_tls_ms` — TLS handshake latency
- `status_monitor_http_pool_idle_connections` — pool idle gauge
- `status_monitor_http_pool_active_connections` — pool active gauge

Tracked in deferred work; out of scope for v1.
