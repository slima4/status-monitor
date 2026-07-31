# Metrics

Prometheus exposition on `metrics_bind` (default `127.0.0.1:9090/metrics`).

## Series

Names below are the on-wire names exactly as registered in
`src/observability/metrics.rs` (`observability::metrics::names`) and
sampled in `src/observability/sampler.rs`. Dashboard queries must use
these names verbatim.

| Name | Type | Purpose |
|---|---|---|
| `uptimepage_checks_total{status}` | counter | checks completed, partitioned by terminal status (`up`/`down`/`degraded`/`error`) |
| `uptimepage_checks_errors_total{kind}` | counter | error breakdown by `kind`; currently only `circuit_open` is emitted (a check skipped because its host breaker was open) |
| `uptimepage_check_redirects_total{outcome}` | counter | HTTP redirect hops (`followed` / `limit_exceeded` / `invalid_location` / `blocked_scheme`) |
| `uptimepage_circuit_breaker_state_changes_total{from,to}` | counter | breaker state transitions |
| `uptimepage_storage_writes_total{store,result}` | counter | batcher flush outcomes |
| `uptimepage_storage_dropped_results_total{reason}` | counter | results dropped before reaching the sink (queue full, etc.) |
| `uptimepage_notifications_total{channel,kind}` | counter | alert notifications dispatched |
| `uptimepage_notifications_failures_total{channel}` | counter | notification dispatches that returned an error |
| `uptimepage_alerts_dropped_total{reason}` | counter | incident paging signals dropped before reaching the escalation engine, by `NotificationReason` (`opened`/`escalated`/`resolved`/`reopened`/`no_data`/`data_resumed`). A lifecycle change never blocks on paging throughput, so a saturated signal channel drops here; the incident row stays in Postgres for the reconcile sweep |
| `uptimepage_notifications_dead_lettered_total{transport}` | counter | incident pages that exhausted all retries without delivering, by transport |
| `uptimepage_telegram_send_deferred_total` | counter | Telegram sends held back by the per-bot/per-chat send budget rather than sent immediately. Sustained growth means the central bot is rate-limit bound |
| `uptimepage_host_throttle_waits_total{kind}` | counter | per-(org,host,port) (`kind=host`) or per-TLD registry (`kind=rdap`) throttle acquire attempts. The `rdap` label covers WHOIS lookups too, since both share the per-TLD bulkhead |
| `uptimepage_host_throttle_drops_total` | counter | host-bulkhead rejections — `kind=host` over-cap checks recorded as `degraded` without firing alerts. RDAP drops do NOT increment this counter; they fall through to the sticky last-good path (see `domain_expiry_stale_served_total`) |
| `uptimepage_rdap_singleflight_total{outcome}` | counter | registry-lookup singleflight outcome per domain (RDAP or WHOIS) — `hit` (cached, no outbound request) or `miss` (fetcher invoked) |
| `uptimepage_domain_expiry_stale_served_total{kind}` | counter | times the domain-expiry executor served a cached last-good answer instead of a fresh probe. `kind` distinguishes the cause: `throttled`, `timeout`, `lookup_error`, or `fresh_error` (no usable last-good — emitted as a real `Error` instead) |
| `uptimepage_flow_runs_total{outcome}` | counter | browser flow runs completed, by `outcome`: `passed`, `failed` (a step failed — the journey is down), `budget` (the whole-run deadline arrived first), `engine` (CDP or the browser process broke), `unconfigured` (the check reached a node with no engine). Only `failed` is a verdict on the target; the rest mean this node stopped paying for an answer |
| `uptimepage_domain_expiry_state_write_failed_total` | counter | failures writing the last-good cache row after a successful probe. Sustained values mean the sticky cache is going cold even though probes succeed — typical cause is Postgres write degradation |
| `uptimepage_scheduler_refresh_failed_total` | counter | registry refresh ticks that returned an error from Postgres. Alert on a sustained rate above your normal noise floor; persistent failures put the scheduler into exponential backoff (capped at 10× the configured refresh interval) and keep workers running with cached `ScheduledTarget` snapshots |
| `uptimepage_rdap_singleflight_slots` | gauge | live entries in the in-process registry-lookup singleflight cache. Bounded under normal load by the set of monitored domains; sudden growth signals a code path feeding non-target domains into the cache |
| `uptimepage_scheduler_consecutive_refresh_failures` | gauge | consecutive registry refresh failures since the last success. Primary alarm signal for a stuck scheduler — page when the value stays above 5 for more than a few minutes. Resets to 0 on the first successful refresh |
| `uptimepage_scheduler_refresh_duration_ms` | histogram | wall-clock duration of one registry refresh tick (Postgres query + decode + DashMap diff). p99 climbing into the hundreds of ms means the current full-scan refresh is starting to strain at scale — the trigger for the deferred incremental-sync work |
| `uptimepage_build_info{version}` | counter | set to 1 once at startup so the endpoint is never empty |
| `uptimepage_check_duration_ms` | histogram | per-check wall time. The `uptimepage_check_*_ms` family is exposed as histogram buckets (not summary quantiles) so percentiles aggregate correctly across regions; query with `histogram_quantile()` |
| `uptimepage_check_dns_ms` | histogram | DNS resolution latency (recorded in the hickory wrapper) |
| `uptimepage_check_connect_ms` | histogram | TCP connect latency (every HTTP check connects fresh) |
| `uptimepage_check_tls_ms` | histogram | TLS handshake latency (per HTTPS check) |
| `uptimepage_check_ttfb_ms` | histogram | time-to-first-byte: request sent to response headers |
| `uptimepage_flow_step_duration_ms{op}` | histogram | wall time of one flow step, by `op` (`goto`/`fill`/`click`/`wait_for`/`assert_text`/`assert_url`). Steps the run never reached are excluded, so the distribution only covers work that happened. A `wait_for` p95 climbing toward the monitor's `step_timeout` is the early warning before the journey starts failing |
| `uptimepage_storage_batch_size` | histogram | flush batch sizes |
| `uptimepage_storage_write_duration_ms` | histogram | flush durations |
| `uptimepage_telegram_send_wait_ms` | histogram | wait imposed on a Telegram send by the send budget before its slot opened |
| `uptimepage_targets_total` | gauge | targets in this process's scheduler registry (sampled). Non-zero only where in-process probing runs; a brain doing agent-only probing reports 0 by design — use `uptimepage_targets_enabled` for the configured-monitor count |
| `uptimepage_targets_enabled{kind}` | gauge | configured enabled monitors counted from Postgres, by `kind`. Slow-cadence inventory gauge, scrape-cached so request load never reaches Postgres; correct on a brain regardless of where probing runs |
| `uptimepage_users_active` | gauge | non-deleted user accounts counted from Postgres. Slow-cadence inventory gauge, scrape-cached |
| `uptimepage_workers_in_flight` | gauge | current worker-pool semaphore depth (sampled). Emitted by every probing process, so on a brain doing agent-only probing the real value is on the agent's `role=probe` series, not the brain's near-zero one |
| `uptimepage_result_queue_depth` | gauge | depth of the result channel buffer (sampled). Present on both the agent (egress to the control plane) and the brain (ingest to storage); separate them by `role` |
| `uptimepage_circuit_breakers_open` | gauge | currently-open breakers (sampled). Probe-side — read the `role=probe` series |
| `uptimepage_monitors_unmonitored` | gauge | monitors whose covering probes have all gone silent (no fresh results), from the silence sweep. Distinct from down: these have no data at all |
| `uptimepage_agent_up{region,agent}` | gauge | 1 if a regional agent checked in within the staleness window, else 0. Emitted by the control plane from `agents.last_seen_at`, so it covers remote agents that Alloy can't scrape. Per-agent series can freeze on agent removal, so alerts use `uptimepage_agents_enabled_down` |
| `uptimepage_agent_last_seen_age_seconds{region,agent}` | gauge | seconds since a regional agent last checked in. Climbs unbounded when an agent goes dark |
| `uptimepage_agents_enabled_down` | gauge | count of enabled regional agents currently past the staleness window. Recomputed every sweep so it never latches. The dead-man signal for a probe region going dark |
| `uptimepage_region_agents_total{region}` | gauge | enabled agents configured for a region — the quorum denominator. Brain-side from the `agents` table |
| `uptimepage_region_agents_up{region}` | gauge | enabled agents in a region fresh within the staleness window — the quorum numerator. `up / total` is the region's health fraction; `up == 0` means the region's agents have all gone stale. Recomputed each sweep; like the per-agent gauges it can freeze if a region's last agent is removed. Covers agents Alloy can't scrape |
| `uptimepage_region_checks_window{region}` | gauge | checks completed in a region over the recent sampling window. Brain-side count from ClickHouse, so it covers remote agents Alloy can't scrape. Only regions with results in the window appear |
| `uptimepage_region_checks_up_window{region}` | gauge | checks that returned up in a region over the recent window. Divide by `uptimepage_region_checks_window` for the success ratio |
| `uptimepage_region_check_latency_p95_ms{region}` | gauge | approximate p95 check latency in a region over the recent window, in ms. Goes stale for a dark region (no new rows), so gate panels on `uptimepage_region_agents_up` |
| `uptimepage_pg_pool_size` | gauge | total connections held in the sqlx Postgres pool (idle + in-use). Bounded above by `storage.postgres.max_connections` |
| `uptimepage_pg_pool_idle` | gauge | connections sitting idle in the Postgres pool. A persistent `idle = 0` alongside `in_use` at the max is the saturation signal |
| `uptimepage_pg_pool_in_use` | gauge | connections checked out of the Postgres pool right now (`size − idle`). Alert on a sustained high `in_use / size` ratio |
| `uptimepage_process_resident_bytes` | gauge | resident set size of the process (`VmRSS`) in bytes. Linux only — absent on non-Linux dev runs. Early-warning signal for slow leaks ahead of the OOM killer |
| `uptimepage_clickhouse_max_part_count_for_partition` | gauge | ClickHouse `MaxPartCountForPartition` (sampled from `system.asynchronous_metrics`). Partition-explosion early warning — climbs toward `parts_to_throw_insert` (default 3000) if a high-cardinality column is added to `PARTITION BY` |
| `uptimepage_http_requests_total{method,route,status}` | counter | inbound HTTP requests handled. `route` is `MatchedPath` (the path-pattern with placeholders) — cardinality bounded by the static router table, never by per-tenant ids. `status` is bucketed `2xx`/`3xx`/`4xx`/`5xx`/`other`; query `sum by (status) (rate(...[5m]))` for the SLO ratio |
| `uptimepage_http_request_duration_ms{method,route}` | histogram | inbound HTTP request latency, exposed as summary quantiles (single web instance, no cross-instance merge). Query `name{quantile="0.99"}` for tail latency per route |
| `uptimepage_http_responses_inflight` | gauge | inbound HTTP requests currently being served. Climbing alongside flat throughput points at handler back-pressure on a downstream pool |
| `uptimepage_ratelimit_drops_total{scope}` | counter | HTTP 429s from the per-org / per-user rate-limit middleware. `scope` is the same string carried in the error body (`per_org_api_writes`, `per_user_bulk_ops`, …) so dashboards can join with `record_quota_event` audit rows. Abuse signal — a tenant hammering the API spikes one scope before shared resources notice |

Scrape interval of 15 s is plenty — counters are written from hot tokio tasks; histograms aggregate per bucket without lock contention.

**Histogram exposition.** Two forms. The `uptimepage_check_*_ms` family is
configured with explicit buckets and exported as a Prometheus **histogram**
(`name_bucket{le="..."}` plus `name_sum` / `name_count`); query it with
`histogram_quantile(0.99, sum(rate(name_bucket[5m])) by (le))` so percentiles
pool correctly across regional agents. Every other `*_ms` / `*_size` histogram
keeps the default exposition, a Prometheus **summary** with precomputed
quantile series (`name{quantile="0.5|0.9|0.95|0.99|0.999"}`) plus `name_sum`
and `name_count`; query those as `name{quantile="0.99"}` directly. Gauges
carry no `org_id` label, these are single-instance operator metrics, not
per-tenant.

**Scrape labels.** The collector stamps two labels the app does not set: `role` (`control-plane` on the brain, `probe` on a regional agent) and, on probe series, `region`. The brain and a probe both emit the prober and pipeline metrics (`check_*`, `workers_in_flight`, `circuit_breakers_open`, `result_queue_depth`, `storage_*`, `process_resident_bytes`), so filter by `role` to read the one you mean rather than summing two processes. The Ops dashboard pins probe panels to `role=probe` and filters them by a `$region` variable; the Business dashboard reads the control-plane-only inventory gauges.

The `uptimepage_region_*` gauges are different: the brain emits them with a `region` label it sets itself (from the `agents` table and from ClickHouse), not a collector-stamped scrape label. They are the per-region surface on a SaaS control plane, where the regional agents are not scraped at all: liveness and quorum from the `agents` table (`region_agents_up` / `_total`), throughput and latency from ClickHouse (`region_checks_window` / `_up_window` / `region_check_latency_p95_ms`). One scrape point, cost scales with regions, not tenants or fleet size.

## OpenTelemetry tracing

Spans are exported over OTLP/HTTP (protobuf) when **both**
`observability.tracing_enabled` and `observability.grafana.enabled` are
`true`. The exporter targets `observability.grafana.otlp_endpoint`
(the OTLP base; `/v1/traces` is appended) and authenticates with
`Authorization: Basic base64(instance_id:api_key)`. The destination is
any OTLP/HTTP collector — Grafana Cloud Tempo, Jaeger, an OpenTelemetry
Collector, etc.

- `api_key` is read only from
  `UPTIMEPAGE_OBSERVABILITY__GRAFANA__API_KEY` — never from a file.
- Sampling is parent-based over a head ratio
  (`grafana.trace_sample_ratio`, default `0.05`); a sampled parent keeps
  its children.
- Resource attributes: `service.name = uptimepage`,
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

## HTTP connection phase timings

Every HTTP check opens a fresh connection (no pool — a monitor probes each target once per interval, so a pool rarely reused a socket, and fresh-connect is what lets the probe attribute time to each phase). `check_dns_ms`, `check_connect_ms`, and `check_tls_ms` are timed during that establishment and `check_ttfb_ms` from request-send to response headers. The same four values are written per-check into ClickHouse, which is what powers the detail-page latency-breakdown chart.
