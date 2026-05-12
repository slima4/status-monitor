# Metrics

Prometheus exposition on `metrics_bind` (default `127.0.0.1:9090/metrics`).

## Series

| Name | Type | Purpose |
|---|---|---|
| `status_monitor_checks_total{status}` | counter | checks completed, partitioned by terminal status (`up`/`down`/`degraded`/`error`) |
| `status_monitor_check_errors_total{kind}` | counter | error breakdown (`timeout` / `connect` / `circuit_open` / `request` / `body` / `transport`) |
| `status_monitor_check_duration_ms` | histogram | per-check wall time |
| `status_monitor_check_dns_ms` | histogram | DNS resolution latency (recorded in the hickory wrapper) |
| `status_monitor_check_connect_ms` | histogram | TCP connect latency (recorded only when a new connection is established) |
| `status_monitor_check_tls_ms` | histogram | TLS handshake latency (recorded only when a new HTTPS connection is established) |
| `status_monitor_check_ttfb_ms` | histogram | time-to-first-byte across the response start |
| `status_monitor_http_pool_idle_connections` | gauge | connections held in the pool but not currently serving a request (sampled) |
| `status_monitor_http_pool_active_connections` | gauge | connections currently serving an in-flight request (sampled) |
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

## HTTP connection phase timings + pool gauges

`check_connect_ms` and `check_tls_ms` are recorded inside `PhaseConnector::call`, which means they fire only when a new connection is established — pooled-connection requests do not produce samples. That's correct semantics: phase timings reflect "establish a connection" cost, not the cost of reusing one.

`http_pool_active_connections` counts in-flight requests via a per-request `ActiveGuard`. `http_pool_idle_connections` is computed as `alive − active`, saturating at zero. Under HTTP/2 a single connection serves many concurrent streams, so `active` can exceed `alive`; idle clamps to zero rather than emit a negative gauge.
