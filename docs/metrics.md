# Metrics

Prometheus exposition on `metrics_bind` (default `127.0.0.1:9090/metrics`).

## Series

Names below are the on-wire names exactly as registered in
`src/observability/metrics.rs` (`observability::metrics::names`) and
sampled in `src/observability/sampler.rs`. Dashboard queries must use
these names verbatim.

| Name | Type | Purpose |
|---|---|---|
| `status_monitor_checks_total{status}` | counter | checks completed, partitioned by terminal status (`up`/`down`/`degraded`/`error`) |
| `status_monitor_checks_errors_total{kind}` | counter | error breakdown by `kind`; currently only `circuit_open` is emitted (a check skipped because its host breaker was open) |
| `status_monitor_check_redirects_total{outcome}` | counter | HTTP redirect hops (`followed` / `limit_exceeded` / `invalid_location` / `blocked_scheme`) |
| `status_monitor_circuit_breaker_state_changes_total{from,to}` | counter | breaker state transitions |
| `status_monitor_storage_writes_total{store,result}` | counter | batcher flush outcomes |
| `status_monitor_storage_dropped_results_total{reason}` | counter | results dropped before reaching the sink (queue full, etc.) |
| `status_monitor_notifications_total{channel,kind}` | counter | alert notifications dispatched |
| `status_monitor_notifications_failures_total{channel}` | counter | notification dispatches that returned an error |
| `status_monitor_alerts_dropped_total{reason}` | counter | alert signals dropped before reaching the engine |
| `status_monitor_build_info{version}` | counter | set to 1 once at startup so the endpoint is never empty |
| `status_monitor_check_duration_ms` | histogram | per-check wall time |
| `status_monitor_check_dns_ms` | histogram | DNS resolution latency (recorded in the hickory wrapper) |
| `status_monitor_check_connect_ms` | histogram | TCP connect latency (recorded only when a new connection is established) |
| `status_monitor_check_tls_ms` | histogram | TLS handshake latency (recorded only when a new HTTPS connection is established) |
| `status_monitor_check_ttfb_ms` | histogram | time-to-first-byte across the response start |
| `status_monitor_storage_batch_size` | histogram | flush batch sizes |
| `status_monitor_storage_write_duration_ms` | histogram | flush durations |
| `status_monitor_http_pool_idle_connections` | gauge | connections held in the pool but not currently serving a request (sampled) |
| `status_monitor_http_pool_active_connections` | gauge | connections currently serving an in-flight request (sampled) |
| `status_monitor_targets_total` | gauge | enabled targets known to the registry (sampled) |
| `status_monitor_workers_in_flight` | gauge | current worker-pool semaphore depth (sampled) |
| `status_monitor_result_queue_depth` | gauge | depth of the result channel buffer (sampled) |
| `status_monitor_circuit_breakers_open` | gauge | currently-open breakers (sampled) |

Scrape interval of 15 s is plenty — counters are written from hot tokio tasks; histograms aggregate per bucket without lock contention.

**Histogram exposition.** `metrics-exporter-prometheus` is installed
without a bucket configuration, so every `*_ms` / `*_size` histogram is
exported as a Prometheus **summary** — quantile time series
(`name{quantile="0.5|0.9|0.95|0.99|0.999"}`) plus `name_sum` and
`name_count`. Query latency as `name{quantile="0.99"}` directly; do
**not** use `histogram_quantile()` / `name_bucket` (no buckets are
emitted). Gauges carry no `org_id` label — these are single-instance
operator metrics, not per-tenant.

## OpenTelemetry tracing

Not yet implemented. The `observability.tracing_enabled` and OTLP
endpoint settings are reserved and currently no spans are exported —
setting `tracing_enabled = true` has no effect today. OTLP trace export
to an OpenTelemetry collector (Grafana Cloud Tempo, Jaeger, etc.) is on
the roadmap; this section will document the live behaviour once it
lands.

## HTTP connection phase timings + pool gauges

`check_connect_ms` and `check_tls_ms` are recorded inside `PhaseConnector::call`, which means they fire only when a new connection is established — pooled-connection requests do not produce samples. That's correct semantics: phase timings reflect "establish a connection" cost, not the cost of reusing one.

`http_pool_active_connections` counts in-flight requests via a per-request `ActiveGuard`. `http_pool_idle_connections` is computed as `alive − active`, saturating at zero. Under HTTP/2 a single connection serves many concurrent streams, so `active` can exceed `alive`; idle clamps to zero rather than emit a negative gauge.
