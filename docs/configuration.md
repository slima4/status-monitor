# Configuration

Defaults live in `config/default.toml`. Every key can be overridden by an environment variable using the prefix `STATUS_MONITOR_` and `__` as the nested separator.

Example: `STATUS_MONITOR_SERVER__API_BIND=0.0.0.0:8080`

Override `STATUS_MONITOR_CONFIG_PATH` to point at an alternate base config file.

## Sections

| Section | Key | Purpose |
|---------|-----|---------|
| `server` | `api_bind`, `metrics_bind` | bind addresses for REST API and Prometheus exporter |
| `runtime` | `worker_threads`, `max_blocking_threads` | Tokio runtime sizing (`0` = `num_cpus`) |
| `checker` | `max_concurrent_checks` | global concurrency cap enforced by worker pool semaphore |
| `checker` | `default_timeout_ms`, `connect_timeout_ms` | client-side timeouts applied to outbound checks |
| `checker` | `default_check_interval_secs` | fallback interval when target spec omits it |
| `http_client` | pool / keep-alive settings | hyper-util Client + connector tuning forwarded to the shared clients |
| `http_client` | `http2_prior_knowledge` | when `true`, client speaks h2c upfront. Default `false`. Used by the loadtest harness |
| `dns` | `cache_size`, `positive_ttl_secs`, `negative_ttl_secs`, `servers` | hickory resolver — point at internal resolvers when needed |
| `security` | `allow_private_targets` | SSRF guard: when `false` (default) any target resolving to loopback / private / link-local / reserved IPs is rejected |
| `circuit_breaker` | `failure_threshold`, `success_threshold`, `open_duration_secs`, `half_open_max_calls` | per-host breaker state machine |
| `storage.postgres` | `url`, `max_connections`, `min_connections`, `acquire_timeout_secs` | target metadata store |
| `storage.clickhouse` | `url`, `database`, `user`, `password`, `batch_size`, `batch_timeout_ms`, `buffer_size` | result sink and pipeline back-pressure |
| `scheduler` | `target_refresh_interval_secs`, `jitter_pct` | how often the registry is reconciled against Postgres, and how much jitter is applied to each target's tick |
| `observability` | `log_level`, `log_format` | tracing-subscriber filter + JSON vs pretty output |
| `observability` | `metrics_enabled`, `gauge_sample_interval_ms` | Prometheus exporter toggle and sampler cadence |
| `observability` | `tracing_enabled`, `otlp_endpoint` | OpenTelemetry export over OTLP/gRPC |

## Tuning notes

- **`max_concurrent_checks`** caps simultaneous in-flight checks. Per-check memory is small (a tokio task plus an in-flight hyper request), so the practical ceiling is set by file descriptors and ephemeral ports rather than RAM.
- **`storage.clickhouse.buffer_size`** is the mpsc capacity between worker pool and batcher. Sized for ~1 s of bursts at peak RPS. Drops increment `storage_dropped_total{reason="queue_full"}` — that metric is your back-pressure signal.
- **`storage.clickhouse.batch_size` vs `batch_timeout_ms`** trade tail latency for throughput. `1000 / 500ms` is a good starting point at ~20k rps.
- **`scheduler.jitter_pct`** prevents synchronized fleet-wide ticks. Default 10% is enough to spread N targets across an interval without making individual schedules unpredictable.
- **`dns.servers`** accepts either bare IPs (`"1.1.1.1"`) or `ip:port` form. Used as is — no system resolver fallback.
- **`security.allow_private_targets`** is the SSRF guard. Default `false` blocks:
  - Loopback (`127.0.0.0/8`, `::1`)
  - RFC1918 private (`10/8`, `172.16/12`, `192.168/16`)
  - Link-local (`169.254/16`, `fe80::/10`) — covers AWS/GCP metadata `169.254.169.254`
  - Carrier-grade NAT (`100.64/10`)
  - IPv6 ULA (`fc00::/7`), discard, IPv4-mapped private, documentation ranges
  - Multicast, broadcast, unspecified, reserved-for-future-use
  The guard runs both at API submission (rejects IP-literal URLs synchronously) and after DNS resolution at connect time (catches DNS rebinding). Flip to `true` for internal monitoring where private targets are the goal — operators are then responsible for network segmentation.
