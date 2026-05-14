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
| `api.cors` | `enabled`, `allowed_origins`, `allowed_methods`, `allow_any_origin` | browser CORS for `/api/v1/*`. Disabled by default. Wildcard only via `allow_any_origin = true` |
| `notifications.slack` | `enabled`, `webhook_url` | Slack incoming-webhook transport. Per-target opt-in via target's `alerts.slack` |
| `notifications.webhook` | `enabled`, `url` | Generic HTTP POST transport — receives the raw `AlertEvent` JSON. Per-target opt-in via target's `alerts.webhook` |
| `notifications.email` | `enabled`, `smtp_host`, `smtp_port`, `smtp_user`, `smtp_password`, `from`, `starttls` | SMTP transport via lettre. Per-target opt-in via target's `alerts.email` (recipients carried per target) |
| `tenancy` | `enabled`, `default_org_slug`, `public_routes_enabled`, `free_tier_owner_org_limit`, `deletion_grace_period_days`, `purge_interval_secs` | Self-host vs SaaS mode + org limits. See [Multi-tenancy mode](#multi-tenancy-mode) below and [docs/multi-tenancy.md](multi-tenancy.md) for the full model |

## Multi-tenancy mode

status-monitor ships from one binary in two modes:

- `tenancy.enabled = false` (default) — **self-host**. The service provisions one org at startup (slug `tenancy.default_org_slug`, default `"default"`) and every request is implicitly scoped to it. No login flow, no orgs API in the user's face. This is the right mode for a team running their own monitoring.
- `tenancy.enabled = true` — **SaaS**. The active org is resolved from the authenticated session. The real session backend (OAuth + password reset + invitations) is on the roadmap; until it lands a stub extractor returns 401 for unauthenticated traffic. Users create orgs through `/api/v1/orgs`, are subject to the three-org owner cap, and only see data tagged with their active `org_id`.

Switching modes is a config flip plus restart; no schema change. The same migrations apply.

### `public_routes_enabled` — the SaaS-mode gotcha

The public status page (`/api/public/v1/status`, `/api/public/v1/badge.svg`, `/api/public/v1/incidents.rss`, `/status`, `/status/incidents/{id}`) is a **single-aggregate** view today: it renders one org's worth of components. Until per-org status routing ships, exposing those routes in SaaS mode would publish every tenant's "public" components to anonymous visitors as one big page.

The hard rule the binary enforces:

| Mode | `public_routes_enabled` | Public routes |
|---|---|---|
| Self-host (`tenancy.enabled = false`) | _ignored_ | mounted (one org → no leak) |
| SaaS (`tenancy.enabled = true`) | `false` (default) | **404** |
| SaaS (`tenancy.enabled = true`) | `true` | mounted — only flip this on after per-org routing is wired |

If you flip `tenancy.enabled = true` for an existing self-host deployment, the public page disappears unless you also flip `public_routes_enabled`. That is intentional: until the per-slug router lands, leaving the page mounted is a data leak.

### Org limits and the purge worker

- `free_tier_owner_org_limit` (default `3`) caps how many orgs a single user can own. Soft-deleted orgs don't count. Enforced inside the membership `INSERT` so concurrent creates can't exceed the cap.
- `deletion_grace_period_days` (default `30`) is how long a soft-deleted org's slug is held and how long the original deleter has to restore it.
- `purge_interval_secs` (default `86400` = 24 h) is the background tick cadence for the soft-delete purge worker. Each tick cascades up to 10 past-grace orgs and drains any pending entries from `clickhouse_purge_queue` (the outbox between PG cascade and ClickHouse `ALTER TABLE DELETE`). See [Soft delete and the 30-day purge](multi-tenancy.md#soft-delete-and-the-30-day-purge) for the full implementation and failure-recovery guarantees.

See [Multi-tenancy](multi-tenancy.md) for the full model, slug rules, and the storage-layer isolation invariants the CI checks enforce.

## Public status page

The public `/status` page has **no global TOML block** in v1 — page cache TTL (10 s), history-strip length (90 days), and recent-incidents horizon (30 days) are hard-coded defaults in `src/public_status/aggregator.rs`. What controls the public surface is per-target:

| Target field | Purpose |
|---|---|
| `public_status` | when `true`, the target is published as a component on `/status` |
| `public_name` | display name (falls back to operator-side `name`) |
| `public_description` | optional one-liner |
| `public_group` | optional group label; ungrouped components render last |
| `public_sort_order` | ASC integer sort within a group |

See [Public status page](public-status.md) for the operator workflow.

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
  - IPv6 transition mechanisms: `2002::/16` (6to4) and `64:ff9b::/96` (NAT64) are decoded to their embedded IPv4 and rejected when the inner IPv4 falls in any blocked range
  The guard runs both at API submission (rejects IP-literal URLs synchronously) and after DNS resolution at connect time (catches DNS rebinding). Flip to `true` for internal monitoring where private targets are the goal — operators are then responsible for network segmentation.
- **`security.credentials_kek_base64`** enables AES-256-GCM encryption of HTTP `basic_auth` and `bearer_token` values inside the `targets.check_spec` JSONB column. Generate with `openssl rand -base64 32`. Each write produces a fresh 12-byte random nonce; the on-disk shape is `{"$enc":"v1:<nonce>:<ciphertext>"}`. When the key is unset the service logs a startup warning and stores credentials plaintext (dev-friendly upgrade path — existing plaintext rows continue to read after a key is provisioned). Rotation and KMS integration are out of scope for the current version; treat the KEK as long-lived and protect it via your secret-management of choice (env file with restricted mode, container secret, etc.). A malformed KEK fails the process at startup.
- **`api.rate_limit`** applies a per-peer-IP token bucket only to `/api/v1/*` routes (`/healthz` and `/readyz` are excluded so liveness probes never see `429`). `per_second` is the refill rate; `burst` is the bucket capacity. Excess requests get `429 Too Many Requests` with a `Retry-After` header. The bucket key is the TCP peer IP — when the service sits behind a reverse proxy, every client appears as the proxy IP, so prefer doing rate limiting at the proxy in that topology. Disabled by default; leave it off and let your reverse proxy enforce limits unless you bind the API directly to the internet.
- **TLS cert checks** (`type = "tls_cert"`) open a dedicated TCP+TLS handshake per probe — they do not reuse the shared HTTP client pool. Recommended `interval >= 3600` so probe traffic stays light. The check accepts any cert chain (the goal is to *report* expiry status, not enforce trust), so an expired or self-signed cert still produces a structured result rather than a generic handshake error.
- **Domain expiry checks** (`type = "domain_expiry"`) query RDAP via a process-shared outbound HTTPS client. The IANA bootstrap registry (`https://data.iana.org/rdap/dns.json`) is fetched lazily on first use and cached for process lifetime — a registry update or a transient bootstrap failure persists until restart. RDAP servers rate-limit clients, so `interval >= 21600` (6 h) is recommended; daily is typical. SSRF guard does not gate these requests because the destination is an IANA-published endpoint, not the user-supplied domain.
- **`notifications.*`** define outbound alert channels. A channel block with `enabled = false` (the default) accepts target opt-ins but produces no notifications — the engine logs a debug line and drops the dispatch. Slack and the generic webhook both POST JSON; Slack receives `{ "text": "..." }`, the generic webhook receives the full `AlertEvent`. Email uses lettre over SMTP — `starttls = true` (default) opens a plain socket and upgrades via STARTTLS, `starttls = false` opens an implicit-TLS socket on `smtp_port` (typically 465). A non-empty `smtp_password` combined with `smtp_port = 25` and `starttls = false` is rejected at startup because it would leak the password in cleartext. Alert state (consecutive-failure counters, per-channel alerting flag) lives in process memory — a restart resets the counters, so a target that was already alerting can re-fire on the next threshold crossing. Per-target opt-in syntax and the fire-once-plus-recovery semantics are documented in [docs/api.md](api.md).
- **`api.cors`** opens `/api/v1/*` to browser-origin access. Each entry in `allowed_origins` must be a full origin (`https://app.example.com`) — wildcards are not parsed; set `allow_any_origin = true` to send `Access-Control-Allow-Origin: *` explicitly. The two are mutually exclusive — combining them or enabling CORS with an empty list aborts startup. `allowed_methods` is echoed in the preflight response (`Access-Control-Allow-Methods`); `Access-Control-Allow-Headers` is fixed to `content-type`, which is what the JSON API needs. `/healthz` and `/readyz` are not wrapped, so liveness probes are unaffected.
