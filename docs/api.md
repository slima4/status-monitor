# REST API

Mounted under `/api/v1` on the configured API bind. JSON in, JSON out. No authentication in v1 — bind to loopback or front it with a reverse proxy you trust.

OpenAPI 3.1 document at `GET /api/openapi.json`; Swagger UI at `GET /docs`.

All responses use `Content-Type: application/json; charset=utf-8`.

## Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/v1/targets` | create one target |
| `POST` | `/api/v1/targets/bulk` | bulk-create up to 10,000 targets |
| `POST` | `/api/v1/targets/bulk-action` | enable / disable / delete / tag-add / tag-remove on many ids |
| `POST` | `/api/v1/targets/test` | run a one-shot check against a `CheckSpec` without persisting |
| `POST` | `/api/v1/targets/{id}/check-now` | run an immediate check using the target's stored credentials |
| `GET` | `/api/v1/targets` | list targets (`limit`, `offset`, `tag`, `enabled`, `q`) — paginated |
| `GET` | `/api/v1/targets/{id}` | get one target |
| `PATCH` | `/api/v1/targets/{id}` | update name, check spec, interval, enabled, tags |
| `DELETE` | `/api/v1/targets/{id}` | delete a target |
| `GET` | `/api/v1/targets/{id}/results` | recent check results (`from`, `to`, `limit`, `offset`) — paginated |
| `GET` | `/api/v1/targets/{id}/uptime` | uptime summary over a range |
| `GET` | `/api/v1/targets/{id}/incidents` | coalesced incident periods (`from`, `to`, `ongoing_only`) — paginated |
| `GET` | `/api/v1/tags` | tag inventory with target counts (`q` prefix) — paginated |
| `GET` | `/api/v1/dashboard/summary` | fleet-wide rollup (5-second in-process cache) |
| `GET` | `/healthz` | liveness — always 200 once the process is up |
| `GET` | `/readyz` | readiness — pings the target store; 503 if unreachable |
| `GET` | `/api/openapi.json` | OpenAPI 3.1 document |
| `GET` | `/docs` | Swagger UI |

## Check specs

Tagged enum, `type` discriminator.

### HTTP

```jsonc
{
  "type": "http",
  "url": "https://example.com/healthz",
  "method": "GET",
  "timeout": 5000,                              // ms, total request budget
  "follow_redirects": false,
  "max_redirects": 0,
  "expected_status": { "kind": "exact", "value": 200 },
  "expected_body_contains": null,               // optional substring match
  "headers": {},
  "body": null,
  "verify_tls": true,
  "basic_auth": null,                           // ["user", "pass"] or null
  "bearer_token": null
}
```

#### Credential redaction

`GET`, `POST`, `PATCH`, and `bulk` responses replace populated `basic_auth` / `bearer_token` fields with the sentinel `"***"`. A `null` field stays `null`, so clients can distinguish "auth is configured" from "no auth". When you `PATCH` a target's `check`, you must re-supply the real credential — a body that contains `"***"` is rejected with `400 Bad Request`. If you only need to change other fields (`name`, `tags`, `enabled`, `interval`), omit `check` from the `PATCH` body. Encryption at rest is gated on [`security.credentials_kek_base64`](configuration.md); the redaction behavior applies in either mode.

`expected_status` variants:

```jsonc
{ "kind": "exact", "value": 200 }
{ "kind": "range", "value": { "min": 200, "max": 299 } }
{ "kind": "one_of", "value": [200, 204] }
```

### TCP

```jsonc
{ "type": "tcp", "host": "db.internal", "port": 5432, "timeout": 2000 }
```

### TLS certificate expiry

```jsonc
{
  "type": "tls_cert",
  "host": "example.com",
  "port": 443,
  "server_name": null,         // optional SNI override; defaults to `host`
  "warn_days": 14,
  "critical_days": 7,
  "timeout": 5000
}
```

Opens a TCP connection, performs a TLS handshake against the host (accepting any presented chain so that expired or self-signed certs can still be inspected), and parses the leaf certificate's `notAfter`. Status mapping:

- `days_remaining < 0` (expired) → `down`
- `days_remaining < critical_days` → `down`
- `days_remaining < warn_days` → `degraded`
- otherwise → `up`

`error` carries a JSON document with `days_remaining`, `not_after`, `subject_common_name`, `issuer_common_name`. A handshake failure (plain-TCP host, network error) returns `error` status with the underlying message. `warn_days` must be strictly greater than `critical_days`. Recommended `interval >= 3600` — every probe opens a fresh TLS connection.

### Domain expiration

```jsonc
{
  "type": "domain_expiry",
  "domain": "example.com",
  "warn_days": 30,
  "critical_days": 7,
  "timeout": 10000
}
```

Queries the [IANA RDAP bootstrap registry](https://data.iana.org/rdap/dns.json) to find the authoritative RDAP server for the domain's TLD, then fetches `/domain/<domain>` and reads the `events[?eventAction == "expiration"]` entry. Status mapping is the same as TLS cert: `< critical_days` → `down`, `< warn_days` → `degraded`, else `up`. Non-`up` results carry a JSON `error` body with `domain`, `days_remaining`, `expiration_date`, and (when present) `registrar`.

The bootstrap registry is fetched lazily on the first lookup and cached for the lifetime of the process. The SSRF guard does not apply — the check's network destination is an IANA-published RDAP server, not the user-supplied domain. Recommended `interval >= 21600` (6 h); RDAP servers rate-limit clients. `warn_days` must be strictly greater than `critical_days`.

## Target payload

```jsonc
{
  "name": "internal-api",
  "check": { /* check spec */ },
  "interval": 30,             // seconds between ticks; min 10 (DB CHECK constraint)
  "enabled": true,
  "tags": ["prod", "tier1"],
  "alerts": { /* optional, see below */ }
}
```

Server returns the full `Target` including `id` (UUIDv7), `created_at`, `updated_at`.

### Alert config

`alerts` is an optional map keyed by channel name. Presence of a channel key opts the target in; omitting the field disables alerting for that target. Channel-specific transport credentials live in [`notifications.*`](configuration.md) — a target opting into a globally-disabled channel logs a debug message and produces no notifications.

```jsonc
"alerts": {
  "slack":   { "after_failures": 3, "notify_recovery": true },
  "webhook": { "after_failures": 6, "notify_recovery": true },
  "email":   { "after_failures": 5, "notify_recovery": false, "to": ["ops@example.com"] }
}
```

- `after_failures` — number of consecutive non-`up` results before the channel fires a `down` notification. Reset to zero on the next `up` result. Must be `>= 1`.
- `notify_recovery` — when `true` (default), an `up` result following a fired `down` emits a `recovered` notification. When `false`, recovery is silent.
- `to` — required for `email`; must contain at least one address. Other channels do not accept this field.

The state machine is fire-once + recovery: while a target is in the `alerting` state, repeat failures do not re-fire. Counters are kept in memory and reset on process restart — after a restart, a target that was already alerting will re-fire when its threshold is next reached.

### Alert validation errors

`POST` and `PATCH` return `400 Bad Request` for:

- `alerts.<channel>: after_failures must be >= 1`
- `alerts.email: 'to' must contain at least one recipient`
- `alerts.email: '<addr>' is not a valid email address` (must contain `@`; full RFC 5321 validation happens at send time)

### Validation errors

`POST` and `PUT` return `400 Bad Request` for:

- Unsupported URL scheme (`url scheme '...' not allowed` — only `http` and `https`)
- Missing URL host, empty TCP host, or TCP/TLS port `0`
- `tls_cert warn_days must be > critical_days`
- `domain_expiry domain must contain a TLD label` (no dot in `domain`)
- `domain_expiry warn_days must be > critical_days`
- **SSRF guard** — `target address ... is in a blocked range`. Triggered when the URL or TCP host is an IP literal that resolves to loopback / private / link-local / reserved space (see [Configuration → `security.allow_private_targets`](configuration.md)). Hostname literals are checked again at connect time after DNS resolution, so DNS rebinding cannot bypass the guard.
- **Redaction sentinel** — `basic_auth contains redaction sentinel — re-supply the real credential` or the equivalent for `bearer_token`. Rejected to prevent a `GET` → `PATCH` round-trip from silently overwriting the stored credential with `"***"`.
- **TLS verification + credentials** — `verify_tls = false cannot be combined with basic_auth or bearer_token over https`. When verification is disabled any host presenting a forged certificate can collect the stored credential on every check interval. Set `verify_tls = true` (recommended) or remove the credential from the target.

## Rate limiting

When [`api.rate_limit.enabled = true`](configuration.md), `/api/v1/*` enforces a per-peer-IP token bucket. Excess requests get `429 Too Many Requests` with a `Retry-After` header (seconds until the next token is available). `/healthz` and `/readyz` are never throttled. The default config disables this layer — front the service with a reverse proxy that rate-limits instead, since the peer IP is the proxy in that topology.

## CORS

Disabled by default. When [`api.cors.enabled = true`](configuration.md), `/api/v1/*` answers preflight `OPTIONS` with `Access-Control-Allow-Origin` (matching `allowed_origins` or `*` when `allow_any_origin = true`), `Access-Control-Allow-Methods` (the configured list), and `Access-Control-Allow-Headers: content-type`. `/healthz` and `/readyz` carry no CORS headers regardless.

## Error envelope

Every 4xx and 5xx response uses one wire shape:

```jsonc
{
  "error": {
    "code": "INVALID_URL_SCHEME",
    "message": "url scheme 'ftp' not allowed",
    "field": "check.url",
    "details": null,
    "trace_id": null
  }
}
```

- `code` is stable, machine-readable, UPPER_SNAKE_CASE. Never repurposed once published.
- `field` is a JSON pointer to the offending input for 400s; `null` for non-field errors.
- `details` carries optional structured context (e.g., `{ "range": "127.0.0.0/8" }` for SSRF rejections).
- `trace_id` is the W3C `traceparent` when tracing is enabled.

Common codes: `INVALID_URL_SCHEME`, `INVALID_URL_FORMAT`, `SSRF_BLOCKED`, `INVALID_INTERVAL`, `INVALID_TIMEOUT`, `INVALID_TCP_PORT`, `INVALID_TCP_HOST`, `INVALID_STATUS_RANGE`, `INVALID_TLS_CERT_PARAMS`, `INVALID_DOMAIN_PARAMS`, `INVALID_TLS_CRED_COMBO`, `INVALID_ALERT_CONFIG`, `REDACTION_SENTINEL`, `BULK_EMPTY`, `BULK_TOO_LARGE`, `BAD_TIME_RANGE`, `TARGET_NOT_FOUND`, `CIRCUIT_OPEN`, `DEPENDENCY_DOWN`, `INTERNAL`.

## Pagination envelope

Every list endpoint returns:

```jsonc
{ "items": [ /* ... */ ], "total": 1240, "limit": 50, "offset": 0 }
```

`limit` defaults to 50 for `/targets` and `/tags`, 1000 for `/results`, 100 for `/incidents`. `limit` is silently capped server-side (10,000 for results, 1,000 for incidents/tags). `total` reflects rows matching the filters, ignoring `limit`/`offset`.

## Results query

`GET /api/v1/targets/{id}/results?from=2026-05-12T00:00:00Z&to=2026-05-12T23:59:59Z&limit=100&offset=0`

- `from` / `to` default to the last 24 h; `to` must be strictly greater than `from` (400 `BAD_TIME_RANGE` otherwise).
- Returns a `PageEnvelope` of `CheckResult` ordered by `timestamp DESC`.

## Uptime query

`GET /api/v1/targets/{id}/uptime?from=…&to=…`

```jsonc
{ "total": 8640, "up": 8635, "down": 0, "degraded": 0, "error": 5, "uptime_pct": 99.94 }
```

## Incidents query

`GET /api/v1/targets/{id}/incidents?from=…&to=…&ongoing_only=false&limit=100&offset=0`

Returns coalesced down / error periods. A contiguous run of bad statuses becomes one incident; an `up` result between two bad runs splits them. Ongoing incidents return `ended_at: null` and `duration_secs: null`.

```jsonc
{
  "items": [
    {
      "id": "01h7m8z4n6v0e1m7v7y6x8x8x8",
      "target_id": "01h7m...",
      "started_at": "2026-05-13T11:30:00.000Z",
      "ended_at":   "2026-05-13T11:35:00.000Z",
      "status":     "down",
      "duration_secs": 300,
      "check_count": 5,
      "error_sample": "connection refused"
    }
  ],
  "total": 1, "limit": 100, "offset": 0
}
```

## Tags inventory

`GET /api/v1/tags?q=prod&limit=100`

Returns every tag currently in use across all targets (enabled or disabled), with target count, sorted by descending count then alphabetical. `q` is a prefix filter for autocomplete.

```jsonc
{ "items": [ { "name": "prod", "count": 12 }, { "name": "staging", "count": 4 } ],
  "total": 2, "limit": 100, "offset": 0 }
```

## Dashboard summary

`GET /api/v1/dashboard/summary` — fleet-wide rollup cached in-process for 5 seconds.

```jsonc
{
  "targets":        { "total": 42, "enabled": 40, "disabled": 2 },
  "current_status": { "up": 38, "down": 1, "degraded": 1, "error": 0, "unknown": 2 },
  "last_24h":       { "checks_total": 50400, "checks_up": 50360, "uptime_pct": 99.92, "incidents": 3 },
  "system":         { "in_flight_checks": 5, "result_queue_depth": 12, "dropped_results_last_5m": 0, "circuit_breakers_open": 0 }
}
```

## On-demand operations

- **`POST /api/v1/targets/test`** — runs one check against a raw `CheckSpec`, no persistence. Same SSRF / URL-scheme / port validation as `POST /targets`. Returns `TestResponse { result, matched_expectations, warnings }`.
- **`POST /api/v1/targets/{id}/check-now`** — runs one check against an existing target using its stored credentials. Result is persisted. Honors the per-host circuit breaker; returns `422 CIRCUIT_OPEN` when the breaker is open. Pass `?force=true` to bypass.
- **`POST /api/v1/targets/bulk-action`** — apply one action atomically to up to 10,000 ids. Partial failure allowed; the response lists `succeeded` and `failed` separately, with per-id `code` + `message`.

```jsonc
{
  "ids": ["01h7m...", "01h7n..."],
  "action": { "type": "disable" }
  // alternatives: { "type": "enable" }, { "type": "delete" },
  //   { "type": "tag_add",    "tags": ["frozen"] },
  //   { "type": "tag_remove", "tags": ["frozen"] }
}
```

## Idempotency

`POST /api/v1/targets/bulk` and `POST /api/v1/targets/bulk-action` accept an optional `Idempotency-Key` header. The server stores the response for 24 hours keyed by `(header value, body hash)`. A retry with the same key and body returns the original response without re-executing. A retry with the same key but a different body executes normally — the body hash is part of the cache key. The cache is in-process; entries are lost on restart.

```http
POST /api/v1/targets/bulk-action HTTP/1.1
Idempotency-Key: 01h7m8z4n6v0e1m7v7y6x8x8x8
Content-Type: application/json

{ "ids": ["..."], "action": { "type": "disable" } }
```
