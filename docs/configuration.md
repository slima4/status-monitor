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
| `security` | `credentials_kek_base64` | 32-byte base64 key encrypting `basic_auth` / `bearer_token` at rest. Empty (default) stores plaintext — dev only |
| `circuit_breaker` | `failure_threshold`, `success_threshold`, `open_duration_secs`, `half_open_max_calls` | per-host breaker state machine |
| `storage.postgres` | `url`, `max_connections`, `min_connections`, `acquire_timeout_secs` | target metadata store |
| `storage.clickhouse` | `url`, `database`, `user`, `password`, `batch_size`, `batch_timeout_ms`, `buffer_size` | result sink and pipeline back-pressure |
| `scheduler` | `target_refresh_interval_secs`, `jitter_pct` | how often the registry is reconciled against Postgres, and how much jitter is applied to each target's tick |
| `observability` | `log_level`, `log_format` | tracing-subscriber filter + JSON vs pretty output |
| `observability` | `metrics_enabled`, `gauge_sample_interval_ms` | Prometheus exporter toggle and sampler cadence |
| `observability` | `tracing_enabled`, `otlp_endpoint` | OpenTelemetry export over OTLP/gRPC |
| `api.rate_limit` | `enabled`, `per_second`, `burst` | per-IP token-bucket rate limiter on `/api/v1/*`. Disabled by default |

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
- **`security.credentials_kek_base64`** enables AES-256-GCM encryption of HTTP `basic_auth` and `bearer_token` values inside the `targets.check_spec` JSONB column. Generate with `openssl rand -base64 32`. Each write produces a fresh 12-byte random nonce; the on-disk shape is `{"$enc":"v1:<nonce>:<ciphertext>"}`. When the key is unset the service logs a startup warning and stores credentials plaintext (dev-friendly upgrade path — existing plaintext rows continue to read after a key is provisioned). Rotation and KMS integration are out of scope for the current version; treat the KEK as long-lived and protect it via your secret-management of choice (env file with restricted mode, container secret, etc.). A malformed KEK fails the process at startup.
- **`api.rate_limit`** applies a per-peer-IP token bucket only to `/api/v1/*` routes (`/healthz` and `/readyz` are excluded so liveness probes never see `429`). `per_second` is the refill rate; `burst` is the bucket capacity. Excess requests get `429 Too Many Requests` with a `Retry-After` header. The bucket key is the TCP peer IP — when the service sits behind a reverse proxy, every client appears as the proxy IP, so prefer doing rate limiting at the proxy in that topology. Disabled by default; leave it off and let your reverse proxy enforce limits unless you bind the API directly to the internet.
