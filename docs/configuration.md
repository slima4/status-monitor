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
| `observability` | `tracing_enabled` | Master on/off for OTLP trace export. Export is active only when this **and** `observability.grafana.enabled` are true |
| `observability.grafana` | `enabled`, `otlp_endpoint`, `instance_id`, `api_key`, `trace_sample_ratio` | OTLP/HTTP trace export to Grafana Cloud / any OTLP collector. `api_key` is env-only. See [Trace export](#trace-export) below |
| `api.rate_limit` | `enabled`, `per_second`, `burst` | per-IP token-bucket rate limiter on `/api/v1/*`. Disabled by default |
| `api.cors` | `enabled`, `allowed_origins`, `allowed_methods`, `allow_any_origin` | browser CORS for `/api/v1/*`. Disabled by default. Wildcard only via `allow_any_origin = true` |
| _notification channels_ | — | Not a config block. Channels are **per-org runtime resources** managed via the [`/api/v1/notification-channels` API](api.md#notification-channels); secrets are sealed at rest with the credentials KEK |
| `tenancy` | `path_based_public_routes`, `subdomain_public_routes`, `free_tier_owner_org_limit`, `deletion_grace_period_days` | Public-status routing shape + org limits. See [Public status routing](#public-status-routing) below and [docs/multi-tenancy.md](multi-tenancy.md) for the full model |
| `retention` | `check_results_days`, `login_attempts_days`, `quota_events_days`, `audit_log_days` | Long-horizon data-retention windows for the daily 03:00-UTC purge job. Every key is bound by the job — no decorative knobs |
| `public_status` | `base_domain`, `cache_max_orgs`, `cache_ttl_secs`, `last_good_ttl_secs`, `logo_dir`, `max_logo_size_bytes`, `allowed_logo_mime_types`, `max_logo_dimension_px`, `default_brand_color`, `default_show_powered_by`, `public_per_ip_rate_limit_per_min` | Per-org public status pages at `{slug}.{base_domain}`. See [Public status page](#public-status-page) below and [Per-org status pages](per-org-status.md) |
| `auth` | `enabled_methods`, `fingerprint_salt`, `public_base_url` | Sign-in methods, HMAC salt for IP/UA hashes, base URL embedded in invitation + magic-link emails. See [Auth configuration](#auth-configuration) below |
| `auth.session` | `idle_timeout_days`, `absolute_timeout_days`, `cookie_name`, `cookie_secure`, `cookie_domain`, `renew_on_use` | Session cookie shape + lifetime. `cookie_secure = true` in production |
| `auth.github` | `client_id`, `client_secret`, `redirect_url`, `scopes`, `http_connect_timeout_ms`, `http_request_timeout_ms` | GitHub OAuth client. Empty client_id disables the GitHub button on `/login` |
| `auth.api_tokens` | `max_per_user`, `prefix_visible_chars` | Cap per user, indexed prefix length for token lookup |
| `auth.invitations` | `expiry_hours`, `max_pending_per_org` | Invitation lifetime and per-org pending cap |
| `auth.magic_link` | `expiry_minutes`, `rate_limit_seconds` | Magic-link token lifetime. Routes only mount when `enabled_methods` includes `"magic_link"` |
| `email` | `provider`, `from_name`, `from_address` | Transactional email backend. `provider` ∈ `"resend" \| "log" \| "memory"` |
| `email.resend` | `api_key` | Required when `email.provider = "resend"` |

## Public status routing

status-monitor ships from one binary as a multi-tenant SaaS. The active org is always resolved from the authenticated session; there is no ambient "default org" and no compile-time self-host mode. A single-tenant deployment is just a SaaS deployment where you sign up as the first user (or seed `users` + `organizations` + `memberships` via a SQL one-shot).

The public status surface is gated by **two** independent flags because path-based and subdomain routing have opposite safety profiles:

- `tenancy.path_based_public_routes` — serve `/status` and `/api/public/v1/*` on the operator host, scoped to the single live org. Useful for a single-tenant deploy (one org, one page). Defaults to `true`. **Must be set to `false` once you have more than one tenant — otherwise every visitor sees the lone org's data regardless of which slug they expected.**
- `tenancy.subdomain_public_routes` — serve one page per org at `{slug}.{public_status.base_domain}` (apex wildcard). Defaults to `false`; requires a well-formed `base_domain`.

| Shape | Recommended flags | Public surface |
|---|---|---|
| Single-tenant | `path_based_public_routes = true` (default) | `/status` on the operator host (one org) |
| Multi-tenant SaaS | `subdomain_public_routes = true`, `path_based_public_routes = false` | `{slug}.{base_domain}` per org |

The binary refuses to boot in the dangerous combinations: `subdomain_public_routes` with an empty or single-label `public_status.base_domain`; or an `auth.session.cookie_domain` that overlaps the status wildcard. Each is a loud panic at startup, not a silent runtime leak. See [Per-org status pages](per-org-status.md) for the full model.

### Org limits and the purge worker

- `free_tier_owner_org_limit` (default `3`) caps how many orgs a single user can own. Soft-deleted orgs don't count. Enforced inside the membership `INSERT` so concurrent creates can't exceed the cap.
- `deletion_grace_period_days` (default `30`) is how long a soft-deleted org's slug is held and how long the original deleter has to restore it.
- The soft-delete purge now runs inside the daily retention job (`src/jobs/retention.rs`) at a fixed 03:00 UTC, not on a configurable interval. Each run cascades up to 10 past-grace orgs, drains any pending entries from `clickhouse_purge_queue` (the outbox between PG cascade and ClickHouse `ALTER TABLE DELETE`), hard-purges past-grace users, then enforces the `[retention]` windows. See [Soft delete and the 30-day purge](multi-tenancy.md#soft-delete-and-the-30-day-purge) for the full implementation and failure-recovery guarantees.

The `[retention]` section sets the long-horizon windows. Defaults: `check_results_days = 30` (enforced by the ClickHouse table `TTL`, kept equal to this number), `login_attempts_days = 180`, `quota_events_days = 90`, `audit_log_days = 730`. Session idle/absolute reaping uses `[auth.session]`; soft-deleted org/user grace uses `tenancy.deletion_grace_period_days`; OAuth-state and magic-link tokens are swept by their own short-cadence jobs.

See [Multi-tenancy](multi-tenancy.md) for the full model, slug rules, and the storage-layer isolation invariants the CI checks enforce.

## Auth configuration

```toml
[auth]
enabled_methods = ["github_oauth"]   # add "magic_link" to mount /auth/magic-link/*
fingerprint_salt = ""                # HMAC salt for IP/UA hashes; rotate-aware
public_base_url = "https://status.example.test"

[auth.session]
idle_timeout_days = 30
absolute_timeout_days = 90
cookie_name = "_sm_session"
cookie_secure = true                 # set false only for plain-HTTP local dev
cookie_domain = ""                   # empty = host-only cookie
renew_on_use = true

[auth.github]
client_id = ""                       # from https://github.com/settings/developers
client_secret = ""
redirect_url = "https://status.example.test/auth/github/callback"
scopes = ["user:email", "read:user"]
http_connect_timeout_ms = 5000
http_request_timeout_ms = 10000

[auth.invitations]
expiry_hours = 168                   # 7 days
max_pending_per_org = 50

[auth.api_tokens]
max_per_user = 25
prefix_visible_chars = 16            # floor; lower values fail boot

[auth.magic_link]
expiry_minutes = 15
rate_limit_seconds = 60                # per-email send throttle; 0 disables

[email]
provider = "log"                     # "resend" in prod, "log" in dev, "memory" in tests
from_name = "Status Monitor"
from_address = "no-reply@example.test"

[email.resend]
api_key = ""                         # required when provider = "resend"
```

The GitHub button on `/login` only renders when `auth.github.client_id`
is set. `auth.enabled_methods` is additive: the GitHub path is always
active in v1 (it's listed as `"github_oauth"` by default); adding
`"magic_link"` mounts the magic-link request/verify endpoints.

`auth.fingerprint_salt` is paired with the `auth_salt_history` table.
Rotating the value mid-deployment refuses to boot unless the override
env var documented in `docs/troubleshooting.md` is set — this is
deliberate so audit-trail breakage is loud.

## Public status page

The `[public_status]` block configures the per-org public surface. It is
load-bearing only when `tenancy.subdomain_public_routes = true`; the
defaults are safe to leave untouched for self-host.

```toml
[public_status]
base_domain = ""                       # REQUIRED when subdomain_public_routes = true
cache_max_orgs = 1000                  # hot + last-good cache bound
cache_ttl_secs = 10                    # per-org rendered-page TTL
last_good_ttl_secs = 3600              # idle eviction for the stale-fallback layer
logo_dir = "/var/lib/status-monitor/logos"
max_logo_size_bytes = 1048576          # 1 MiB byte ceiling (pre-decode)
allowed_logo_mime_types = ["image/png", "image/jpeg", "image/webp"]
max_logo_dimension_px = 1200           # larger uploads are downscaled; decode
                                       # is also allocation-bounded (bomb guard)
default_brand_color = "#3b82f6"        # used when an org sets no colour
default_show_powered_by = true
public_per_ip_rate_limit_per_min = 60  # in-app limit behind the Caddy-side one
```

| Key | Purpose |
|---|---|
| `base_domain` | parent domain for `{slug}.{base_domain}`. Must be multi-label; boot fails on empty/single-label when subdomain routing is on |
| `cache_max_orgs` / `cache_ttl_secs` | per-org page cache size and freshness window |
| `last_good_ttl_secs` | how long an idle org's last-known-good snapshot is retained before eviction |
| `logo_dir`, `max_logo_size_bytes`, `allowed_logo_mime_types`, `max_logo_dimension_px` | logo upload storage and limits |
| `default_brand_color`, `default_show_powered_by` | fallbacks when an org leaves branding unset |
| `public_per_ip_rate_limit_per_min` | second-layer rate limit behind the reverse proxy's |

History-strip length (90 days) and the recent-incidents horizon (30 days)
remain hard-coded defaults in `src/public_status/aggregator.rs`. What an
org publishes is per-target:

| Target field | Purpose |
|---|---|
| `public_status` | when `true`, the target is published as a component on `/status` |
| `public_name` | display name (falls back to operator-side `name`) |
| `public_description` | optional one-liner |
| `public_group` | optional group label; ungrouped components render last |
| `public_sort_order` | ASC integer sort within a group |

See [Public status page](public-status.md) for the operator workflow and
[Per-org status pages](per-org-status.md) for the SaaS subdomain model.

## Trace export

OpenTelemetry spans are exported over OTLP/HTTP (protobuf) when **both**
`observability.tracing_enabled` and `observability.grafana.enabled` are
`true`. Disabled by default and zero-cost when off.

```toml
[observability]
tracing_enabled = false                # master on/off for trace export

[observability.grafana]
enabled = false                        # second switch; both must be true
otlp_endpoint = ""                     # OTLP base, no /v1/traces suffix; e.g.
                                       # https://otlp-gateway-<zone>.grafana.net/otlp
instance_id = ""                       # Grafana Cloud numeric instance / stack id
trace_sample_ratio = 0.05              # parent-based head sampling, [0.0, 1.0]
# api_key                              # NEVER in TOML — env var only (below)
```

| Key | Purpose |
|---|---|
| `tracing_enabled` | master switch; with `grafana.enabled` gates all export |
| `grafana.enabled` | second switch (kept separate so the block is inert until explicitly turned on) |
| `grafana.otlp_endpoint` | OTLP/HTTP **base** URL; the service appends `/v1/traces` (a value already ending in it is left as-is). Empty fails boot when export is on |
| `grafana.instance_id` | basic-auth username (Grafana Cloud instance id). Empty fails boot when export is on |
| `grafana.api_key` | basic-auth password. **Env-only**: `STATUS_MONITOR_OBSERVABILITY__GRAFANA__API_KEY`. Never read from a config file; redacted in any serialised config |
| `grafana.trace_sample_ratio` | head sampling ratio under a parent-based sampler. Must be in `[0.0, 1.0]` or boot fails |

Auth is `Authorization: Basic base64(instance_id:api_key)`. Resource
attributes `service.name = status-monitor` and `service.version` are
attached. The batch exporter is flushed and stopped on graceful
shutdown. A transport build failure logs a warning and the service
continues without traces — telemetry never takes down monitoring.
Inconsistent settings (export on with a missing endpoint / instance /
key, or an out-of-range ratio) are a clean startup config error.

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
- **Notification channels** are no longer global config. They are per-org runtime resources (Slack incoming webhook, generic HTTP webhook, Telegram bot) created via the [`/api/v1/notification-channels` API](api.md#notification-channels); a target binds them by id in its `alerts` array. Transport secrets are sealed at rest with the credentials KEK and never echoed back. Slack POSTs `{ "text": "..." }`, the generic webhook POSTs the full `AlertEvent` (plus any configured custom headers). Alert state (consecutive-failure counters, per-`(target, channel)` alerting flag) lives in process memory — a restart resets the counters, so a target that was already alerting can re-fire on the next threshold crossing. The binding syntax and fire-once-plus-recovery semantics are documented in [docs/api.md](api.md#alert-config).
- **`api.cors`** opens `/api/v1/*` to browser-origin access. Each entry in `allowed_origins` must be a full origin (`https://app.example.com`) — wildcards are not parsed; set `allow_any_origin = true` to send `Access-Control-Allow-Origin: *` explicitly. The two are mutually exclusive — combining them or enabling CORS with an empty list aborts startup. `allowed_methods` is echoed in the preflight response (`Access-Control-Allow-Methods`); `Access-Control-Allow-Headers` is fixed to `content-type`, which is what the JSON API needs. `/healthz` and `/readyz` are not wrapped, so liveness probes are unaffected.
