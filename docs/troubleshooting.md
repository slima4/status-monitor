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

## Targets reporting `degraded` with `throttled: host concurrency cap`

One tenant has more concurrent monitors at the same `(host, port)` than `checker.per_host_max_inflight` allows (default 2). Over-cap checks are recorded `degraded` instead of running. No alert fires — the upstream is fine. Either spread the targets across more hosts, raise the cap, or rely on jitter to thin the burst. Watch `status_monitor_host_throttle_drops_total` to size the cap against real traffic.

## `domain_expiry` results show `served_stale: …`

The fresh RDAP probe failed (throttle, timeout, registry 5xx, network blip) but the executor served the most recent successful answer from `domain_expiry_state` instead of flipping the monitor red. The status reflects the cached `expiry_at`. For Up the `error` field stays empty (the customer-facing surface shows nothing unusual); for Degraded/Down it carries `served_stale: last_verified_age_secs=…; refresh_failed=<kind>` plus the cached details so operators can distinguish a stale serve from a fresh probe.

Inspect the failure kind via `status_monitor_domain_expiry_stale_served_total{kind}`:

- `kind="throttled"` — per-TLD RDAP bulkhead rejected this probe. Raise `checker.rdap_max_inflight` if rampant, but the cap is also the IANA-friendliness lever.
- `kind="timeout"` — the registry took longer than `check.timeout` (per-target). Either bump the per-check timeout or wait — most registries recover in minutes.
- `kind="lookup_error"` — registry returned a non-2xx (often 404 or 5xx). If a specific TLD is stuck on 5xx, the registry is having an incident; rows keep streaming as `served_stale` until 7 days have passed.
- `kind="fresh_error"` — no usable last-good (first probe, or the cached row is older than 7d). A real `CheckStatus::Error` is emitted and is alert-eligible.

## `domain_expiry` results have flipped to real `Error` after days of `served_stale`

The cached row in `domain_expiry_state` is older than the 7-day staleness ceiling, so the executor stopped masking the registry outage. Either the registry has been down for that long (act on it), or this target's interval is so long that probes haven't run in a week. Check `last_success_at` in `domain_expiry_state` for the target.

## TLS errors against internal hosts

Set `verify_tls: false` on the offending target. The check executor picks between a verifying and a non-verifying hyper-util client based on the flag — both share the same DNS cache and connection-pool sizing.

## `400 Bad Request` on POST /targets — `target address ... is in a blocked range`

SSRF guard rejected the target. The URL or TCP host resolves to a private / loopback / link-local / reserved IP. Verify the resolved address is what you expect. To monitor private infrastructure deliberately, set `security.allow_private_targets = true` and ensure network segmentation prevents abuse.

## Check fails with `all resolved addresses for 'host' are in blocked ranges`

DNS returned only private IPs for a target the API previously accepted (hostname literal). Either the record changed or DNS rebinding is in play. The connect-time guard refuses to continue. Either fix DNS or, deliberately, enable `security.allow_private_targets`.

## `credential decryption failed` errors in logs

The KEK loaded at startup can no longer decrypt rows written with a different KEK. Either `security.credentials_kek_base64` was rotated without re-encrypting existing rows, or the wrong key was supplied. Compare the configured KEK against the one used to write the affected targets — there is no automatic rotation. Recovery options:

- Restore the original KEK.
- Delete and re-create the affected targets (the row decrypts cleanly when overwritten via `PATCH` or `POST` under the new key).

## Startup fails with `invalid credentials_kek_base64`

The supplied key is not 32 bytes after base64 decode, or the string is not valid base64. Generate a fresh key with `openssl rand -base64 32`. URL-safe and standard base64 both decode.

## `400 Bad Request` on PATCH /targets/{id} — `basic_auth contains redaction sentinel`

A client read the target back (where credentials are returned as `"***"`) and `PATCH`ed the full `check` body without re-supplying the real credential. Either send the real value, or omit `check` entirely from the `PATCH` body if only other fields are changing.

## `429 Too Many Requests` on `/api/v1/*`

Per-IP rate limiter is active and the bucket is empty. Read the `Retry-After` header for the wait time, or raise `api.rate_limit.{per_second, burst}`. If every client appears to share one bucket, the service is sitting behind a reverse proxy and the peer IP is the proxy — disable the in-app limiter (`api.rate_limit.enabled = false`) and let the proxy enforce per-client limits instead.

## ClickHouse insert fails with `SchemaMismatch`

Almost always a Row-derive mismatch on UUID, Enum8, or DateTime64 column types:

- UUID columns require `#[serde(with = "clickhouse::serde::uuid")]` on the field
- Enum8 columns require an `i8` field, not `&str`
- DateTime64 filter binds in `WHERE` clauses need wrapping in `fromUnixTimestamp64Milli(?)` — raw `i64` won't coerce to DateTime64 in CH expressions

## Loadtest reports `connect` errors at high concurrency

Loopback ephemeral port exhaustion or kernel SYN backlog overflow. See [loadtest.md](loadtest.md) — set `MOCK_PORTS=64`, `RAMP_SECS=30`, or enable `HTTP2=1`.
