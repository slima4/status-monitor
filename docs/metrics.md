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
| `status_monitor_host_throttle_waits_total{kind}` | counter | per-(org,host,port) (`kind=host`) or per-TLD RDAP (`kind=rdap`) throttle acquire attempts |
| `status_monitor_host_throttle_drops_total` | counter | host-bulkhead rejections — `kind=host` over-cap checks recorded as `degraded` without firing alerts. RDAP drops do NOT increment this counter; they fall through to the sticky last-good path (see `domain_expiry_stale_served_total`) |
| `status_monitor_rdap_singleflight_total{outcome}` | counter | RDAP singleflight outcome per domain — `hit` (cached, no outbound request) or `miss` (fetcher invoked) |
| `status_monitor_domain_expiry_stale_served_total{kind}` | counter | times the domain-expiry executor served a cached last-good answer instead of a fresh probe. `kind` distinguishes the cause: `throttled`, `timeout`, `lookup_error`, or `fresh_error` (no usable last-good — emitted as a real `Error` instead) |
| `status_monitor_domain_expiry_state_write_failed_total` | counter | failures writing the last-good cache row after a successful probe. Sustained values mean the sticky cache is going cold even though probes succeed — typical cause is Postgres write degradation |
| `status_monitor_scheduler_refresh_failed_total` | counter | registry refresh ticks that returned an error from Postgres. Alert on a sustained rate above your normal noise floor; persistent failures put the scheduler into exponential backoff (capped at 10× the configured refresh interval) and keep workers running with cached `ScheduledTarget` snapshots |
| `status_monitor_rdap_singleflight_slots` | gauge | live entries in the in-process RDAP singleflight cache. Bounded under normal load by the set of monitored domains; sudden growth signals a code path feeding non-target domains into the cache |
| `status_monitor_scheduler_consecutive_refresh_failures` | gauge | consecutive registry refresh failures since the last success. Primary alarm signal for a stuck scheduler — page when the value stays above 5 for more than a few minutes. Resets to 0 on the first successful refresh |
| `status_monitor_scheduler_refresh_duration_ms` | histogram | wall-clock duration of one registry refresh tick (Postgres query + decode + DashMap diff). p99 climbing into the hundreds of ms means the current full-scan refresh is starting to strain at scale — the trigger for the deferred incremental-sync work |
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
| `status_monitor_pg_pool_size` | gauge | total connections held in the sqlx Postgres pool (idle + in-use). Bounded above by `storage.postgres.max_connections` |
| `status_monitor_pg_pool_idle` | gauge | connections sitting idle in the Postgres pool. A persistent `idle = 0` alongside `in_use` at the max is the saturation signal |
| `status_monitor_pg_pool_in_use` | gauge | connections checked out of the Postgres pool right now (`size − idle`). Alert on a sustained high `in_use / size` ratio |
| `status_monitor_process_resident_bytes` | gauge | resident set size of the process (`VmRSS`) in bytes. Linux only — absent on non-Linux dev runs. Early-warning signal for slow leaks ahead of the OOM killer |

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

Spans are exported over OTLP/HTTP (protobuf) when **both**
`observability.tracing_enabled` and `observability.grafana.enabled` are
`true`. The exporter targets `observability.grafana.otlp_endpoint`
(the OTLP base; `/v1/traces` is appended) and authenticates with
`Authorization: Basic base64(instance_id:api_key)`. The destination is
any OTLP/HTTP collector — Grafana Cloud Tempo, Jaeger, an OpenTelemetry
Collector, etc.

- `api_key` is read only from
  `STATUS_MONITOR_OBSERVABILITY__GRAFANA__API_KEY` — never from a file.
- Sampling is parent-based over a head ratio
  (`grafana.trace_sample_ratio`, default `0.05`); a sampled parent keeps
  its children.
- Resource attributes: `service.name = status-monitor`,
  `service.version` = the build version.
- Disabled by default and **zero-cost when off**: no exporter is built,
  no network egress, no per-check overhead.
- A batch exporter ships spans in the background; it is flushed and
  stopped on graceful shutdown so the final spans are not lost. A
  transport build failure logs a warning and the service continues
  without traces — telemetry never takes down monitoring.

Inconsistent settings (export on but endpoint/instance/key missing, or
a sample ratio outside `[0.0, 1.0]`) fail fast at startup as a config
error, not a runtime surprise. See
[Configuration](configuration.md) for the keys and env overrides.

## HTTP connection phase timings + pool gauges

`check_connect_ms` and `check_tls_ms` are recorded inside `PhaseConnector::call`, which means they fire only when a new connection is established — pooled-connection requests do not produce samples. That's correct semantics: phase timings reflect "establish a connection" cost, not the cost of reusing one.

`http_pool_active_connections` counts in-flight requests via a per-request `ActiveGuard`. `http_pool_idle_connections` is computed as `alive − active`, saturating at zero. Under HTTP/2 a single connection serves many concurrent streams, so `active` can exceed `alive`; idle clamps to zero rather than emit a negative gauge.
