# REST API

Mounted under `/api/v1` on the configured API bind. JSON in, JSON out. No authentication in v1 — bind to loopback or front it with a reverse proxy you trust.

OpenAPI 3.1 document at `GET /api/openapi.json`; Swagger UI at `GET /docs`.

All responses use `Content-Type: application/json; charset=utf-8`.

### Response headers

- `POST /api/v1/targets` (201) sets `Location: /api/v1/targets/{id}` so clients can follow up without re-deriving the path.
- `Cache-Control` is stamped on every `/api/v1/*` response:
  - mutations (POST / PATCH / DELETE) → `no-store`
  - `/api/v1/dashboard/summary` → `private, max-age=5` (matches the server-side cache)
  - all other reads → `private, max-age=10`

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
| `GET` | `/api/v1/targets/{id}/results` | recent check results (`from`, `to`, `limit`, `offset`, `region`) — paginated |
| `GET` | `/api/v1/targets/{id}/latency` | bucketed latency series (`from`, `to`, `region`) — server-side quantiles + per-phase means |
| `GET` | `/api/v1/targets/{id}/latency/by-region` | per-region latency series (`from`, `to`) — one series per region, for overlay charts |
| `GET` | `/api/v1/targets/{id}/uptime` | uptime summary over a range (`from`, `to`, `region`) |
| `GET` | `/api/v1/targets/{id}/incidents` | coalesced incident periods (`from`, `to`, `ongoing_only`) — paginated |
| `POST` | `/api/v1/targets/{id}/shares` | mint a read-only share link; returns the share (token included) |
| `GET` | `/api/v1/targets/{id}/shares` | list a monitor's live share links (token included, re-copyable) |
| `DELETE` | `/api/v1/targets/{id}/shares/{share_id}` | revoke a share link |
| `GET` | `/api/v1/tags` | tag inventory with target counts (`q` prefix) — paginated |
| `GET` | `/api/v1/dashboard/summary` | per-org rollup (5-second in-process cache, keyed by `OrgId`) |
| `GET` | `/healthz` | liveness — always 200 once the process is up |
| `GET` | `/readyz` | readiness — pings the target store; 503 if unreachable |
| `GET` | `/api/openapi.json` | OpenAPI 3.1 document |
| `GET` | `/docs` | Swagger UI |

### Instance-admin and agent surfaces

Two surfaces sit outside `/api/v1` with their own auth, used only for multi-region deployments:

- `/operator/*` — instance-admin regions + agents CRUD, gated by a static bearer secret (`UPTIMEPAGE_OPERATOR__ADMIN_TOKEN`); `404`s when unset.
- `/api/agent/*` — the pull/ingest endpoints an agent uses, authenticated by its `sm_agent_…` token (not a tenant `api_token`).

Both are documented in [Multi-region probes](multi-region.md).

### Operator endpoints (maintenance + incident narration)

These mutate the public surface; they live under the same auth boundary as
`/api/v1/targets`. Operator workflow + validation rules in
[Public status page](public-status.md).

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/v1/maintenance` | schedule a maintenance window |
| `GET` | `/api/v1/maintenance` | list windows (`status=active\|upcoming\|past\|all`, paginated) |
| `GET` | `/api/v1/maintenance/{id}` | get one window |
| `PATCH` | `/api/v1/maintenance/{id}` | edit title / description / time range / components (rejected after `ends_at`) |
| `DELETE` | `/api/v1/maintenance/{id}` | cancel a window |
| `PATCH` | `/api/v1/incidents/{id}` | update narration: `public_title`, `public_description`, `severity` (JSON `null` clears, omit to leave alone) |
| `POST` | `/api/v1/incidents/{id}/updates` | append a status update — `phase` ∈ `investigating`/`identified`/`monitoring`/`resolved`/`postmortem`, `message` ≤ 2 000 chars |

### Operator endpoints (status pages)

An org owns one or more public status pages, each with its own slug, branding,
and curated set of monitors. Reads are open to any active member; every mutation
is owner-only. Scoped to the caller's active org (a foreign page id is 404).
Adding a monitor already on the page returns 409 `COMPONENT_ALREADY_ON_PAGE` —
edit it with `PATCH`. Model + caps in [Per-org status pages](per-org-status.md).

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/v1/status-pages` | list this org's pages |
| `POST` | `/api/v1/status-pages` | create a page (capped at `max_status_pages`; slug globally unique) |
| `GET` | `/api/v1/status-pages/{id}` | one page + its live URL and logo URL |
| `PATCH` | `/api/v1/status-pages/{id}` | rename, change slug, publish/unpublish, edit branding |
| `DELETE` | `/api/v1/status-pages/{id}` | delete the page |
| `GET` | `/api/v1/status-pages/{id}/components` | the monitors curated onto the page |
| `POST` | `/api/v1/status-pages/{id}/components` | add a monitor (distinct-target cap `max_public_components`) |
| `PATCH` | `/api/v1/status-pages/{id}/components/{target_id}` | per-page `public_name` / `public_description` / `public_group` (JSON `null` clears) |
| `DELETE` | `/api/v1/status-pages/{id}/components/{target_id}` | remove a monitor from the page |
| `POST` | `/api/v1/status-pages/{id}/components/reorder` | set component order |
| `POST` | `/api/v1/status-pages/{id}/logo` | upload a logo (multipart) |
| `DELETE` | `/api/v1/status-pages/{id}/logo` | remove the logo |

### Public status endpoints

Unauthenticated; mounted at `/api/public/v1/*` and bypassed at Caddy via the
`@public` matcher (see [Deployment](deployment.md#public-status-surface)).
Each response carries `Cache-Control: public, max-age=10,
stale-while-revalidate=30`. A monitor not curated onto the page being
served is invisible on every public surface — direct lookups return 404
and it never appears in any list. Wire types literally cannot serialise
sensitive target fields (`url`, `headers`, `basic_auth`, `bearer_token`).

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/status` | server-rendered HTML status page (`?fragment=1` returns the dynamic region only) |
| `GET` | `/status/incidents/{id}` | per-incident detail page |
| `GET` | `/api/public/v1/status` | the same data as `/status` in JSON |
| `GET` | `/api/public/v1/components/{id}/history` | per-component 90-day history (`days` query, default 90, max 90) |
| `GET` | `/api/public/v1/incidents` | recent public incidents (paginated) |
| `GET` | `/api/public/v1/incidents/{id}` | one public incident with its update timeline |
| `GET` | `/api/public/v1/incidents.rss` | RSS 2.0 feed of recent incidents |
| `GET` | `/api/public/v1/maintenance` | active + upcoming maintenance windows |
| `GET` | `/api/public/v1/badge.svg` | embeddable SVG status badge (overall, or `?component={id}`) |

See [Public status page](public-status.md) for the operator workflow and
the per-page component fields (`public_name`, `public_description`,
`public_group`, `sort_order`) that drive what's published.

### Operator endpoints (share links)

A share link is a capability URL that renders one monitor's full read-only detail view to anyone who has it, no account. Managing share links — mint, list, revoke — is a monitor action gated on member-level `targets:write` (not owner-only); listing returns the live token so a read-only caller can't harvest working public links. Scoped to the caller's active org (a foreign monitor id is 404). `expires_at` is optional; omit it for a link that never expires. The public surface those tokens unlock is documented in [Share links](share-links.md).

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/v1/targets/{id}/shares` | mint a share; body `{ "label"?, "expires_at"? }`, returns the `MonitorShare` |
| `GET` | `/api/v1/targets/{id}/shares` | list live (non-revoked) shares |
| `DELETE` | `/api/v1/targets/{id}/shares/{share_id}` | revoke immediately — the link 404s on its next request |

Both `POST` and `GET` return the `token`; build the link as `/m/{token}` (prepend your origin). The token stays re-copyable — it is stored encrypted at rest (the app KEK, same as `basic_auth`/`bearer_token`); the public resolve path matches on a separate hash, so a hot link never triggers a decrypt. `token` is `null` only when a row was sealed under a KEK that is no longer configured. Two plan caps apply (columns on `plans`, overridable per-org via `plan_overrides`): `max_share_links_per_monitor` (active links on one monitor) and `max_shared_monitors` (distinct monitors in the org that have any link). The free plan is **1** and **2**. Exceeding either is `422 QUOTA_EXCEEDED` (the body names the `quota`). A label longer than 80 characters is `400 SHARE_LABEL_INVALID`; an `expires_at` in the past is `400 INVALID_EXPIRY`.

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

#### Rate-limited responses

A response with `429 Too Many Requests` or `503 Service Unavailable` is recorded as `degraded`, not `down` — the upstream is telling us "I'm here, back off." The `error` field carries `rate-limited <code> (Retry-After: <value>)` when the header is present so operators can size the polling interval against what the upstream actually wants. A check that explicitly accepts 429 / 503 via `expected_status` is honored first and stays `up`.

Some third-party APIs rate-limit by source IP regardless. GitHub's unauthenticated REST API is the canonical case: 60 req/h per IP, 5 000 req/h with a token in the `Authorization` header. Poll those endpoints at ≥ 300 s, or attach the token via a header in this spec.

#### Per-host throttle

The worker side caps the number of concurrent checks one tenant can fan at the same `(host, port)` so a burst of monitors against one upstream doesn't look like a probe. When the cap is reached, the over-cap check is recorded as `degraded` with `error="throttled: host concurrency cap"` and **no alert fires** — the upstream is fine, the back-pressure is operator-side. The cap is per-tenant: one customer's burst never starves another customer's monitor of the same host. Default cap is two in-flight per `(org, host, port)`; tune via `checker.per_host_max_inflight`. RDAP queries (domain expiry) carry their own per-TLD cap via `checker.rdap_max_inflight`.

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

`error` carries a JSON document with `days_remaining`, `not_after`, `subject_common_name`, `issuer_common_name`. A handshake failure (plain-TCP host, network error) returns `error` status with the underlying message. `warn_days` must be strictly greater than `critical_days`. Floor is `interval >= 3600` (enforced); default for a new monitor is `86400` (daily).

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

The bootstrap registry is fetched lazily on the first lookup and cached for the lifetime of the process. The SSRF guard does not apply — the check's network destination is an IANA-published RDAP server, not the user-supplied domain. Floor is `interval >= 3600` (enforced); default for a new monitor is `86400` (daily). RDAP servers rate-limit clients — keep this near daily, not hourly. `warn_days` must be strictly greater than `critical_days`.

## Target payload

```jsonc
{
  "name": "internal-api",
  "check": { /* check spec */ },
  "interval": 60,             // seconds between ticks; effective floor is
                              // max(plan.min_check_interval_secs, kind_min).
                              // kind_min is 10 for http/tcp/dns and 3600 for
                              // tls_cert/domain_expiry. Plan-free min = 60.
                              // 10 is the absolute DB CHECK hard floor.
  "enabled": true,
  "tags": ["prod", "tier1"],
  "alerts": { /* optional, see below */ }
}
```

Server returns the full `Target` including `id` (UUIDv7), `created_at`, `updated_at`, and `write_source`.

`write_source` is a read-only field recording where the resource was last
written from: `ui`, `api`, or `terraform` (decided server-side from the
request, never the body — sending it is ignored). It also appears on
notification channels and maintenance windows, and drives the "managed by"
badge in the web UI. A write through any endpoint restamps it, so it reflects
the most recent author.

### Alert config

`alerts` is an optional array of channel bindings. Each binding references a
notification channel by `channel_id` (see [Notification channels](#notification-channels)).
An empty/omitted array disables alerting for that target.

```jsonc
"alerts": [
  { "channel_id": "0192a1ce-0000-7000-8000-000000000001", "after_failures": 3, "notify_recovery": true },
  { "channel_id": "0192a1ce-0000-7000-8000-000000000002", "after_failures": 6, "notify_recovery": false }
]
```

- `channel_id` — id of a notification channel owned by the **same org**. A binding to an unknown or another tenant's channel is rejected.
- `after_failures` — consecutive non-`up` results before the binding fires a `down` notification. Reset to zero on the next `up`. Must be `>= 1`.
- `notify_recovery` — when `true` (default), an `up` result following a fired `down` emits a `recovered` notification. When `false`, recovery is silent.

The state machine is fire-once + recovery, tracked per `(target, channel)`: while in the `alerting` state, repeat failures do not re-fire. Counters are kept in memory and reset on process restart — after a restart, a target that was already alerting will re-fire when its threshold is next reached.

### Alert validation errors

`POST` and `PATCH` return `400 Bad Request` (`INVALID_ALERT_CONFIG`) for:

- `after_failures must be >= 1`
- a duplicate `channel_id` in the array
- `notification channel <id> does not exist` — unknown id, or one owned by another org

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

## Notification channels

Per-org delivery destinations that targets bind to via their `alerts` array.
Org scoping is implicit in the caller's authenticated context — one tenant can
never read, mutate, or test another's channels.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/v1/notification-channels` | Create a channel (201 + `Location`) |
| `GET` | `/api/v1/notification-channels` | List the org's channels |
| `GET` | `/api/v1/notification-channels/{id}` | Get one |
| `PATCH` | `/api/v1/notification-channels/{id}` | Partial update |
| `DELETE` | `/api/v1/notification-channels/{id}` | Delete (204) |
| `POST` | `/api/v1/notification-channels/{id}/test` | Send a synthetic test alert |

```jsonc
{
  "name": "Ops Slack",
  "enabled": true,
  "config": { "type": "slack", "webhook_url": "https://hooks.slack.com/services/T/B/XXXX" }
}
```

`config` is `type`-tagged. Supported transports:

- `slack` — `{ "type": "slack", "webhook_url": "https://…" }` (incoming webhook; posts `{ "text": "…" }`)
- `webhook` — `{ "type": "webhook", "url": "https://…", "headers": { … } }` (POSTs the raw `AlertEvent` JSON; optional custom headers)
- `telegram` — `{ "type": "telegram", "bot_token": "…", "chat_id": "…" }`

Behaviour:

- **Secrets sealed at rest** with the credentials KEK; **never echoed back**. Every read path masks secret-bearing fields with `***` (the webhook URL is masked whole — it can carry a token; header *names* and `chat_id` are kept so the UI stays useful).
- **Redaction-sentinel guard**: submitting a `config` that still contains `***` returns `400 REDACTION_SENTINEL`. Omit `config` on `PATCH` to keep the stored secret unchanged.
- **Validation** (`400`): `webhook`/`slack` URLs must be `https`; `telegram` requires non-empty `bot_token` and `chat_id`; channel `name` is required and ≤ 100 chars.
- **Quota**: capped per org by the plan's `max_notification_channels` (atomic, advisory-locked). A duplicate name within the org is `422 CHANNEL_NAME_TAKEN`; the cap is `422 CHANNEL_QUOTA_EXCEEDED`.
- **Test send** delivers one clearly-labelled synthetic alert through the channel's transport (works on a disabled channel). A transport failure is `422 CHANNEL_TEST_FAILED`.

## Rate limiting

`/api/v1/*` is rate-limited per authenticated subject — by `(org, category)` and by `(user, category)`, whichever trips first — with the per-minute budgets taken from the org's plan. Categories: `api_writes` (POST/PATCH/DELETE), `api_reads` (GET/HEAD/OPTIONS), `bulk_ops` (`/bulk*`), `test_now` (`/test`), `check_now` (`/check-now`). Exceeding a budget returns `429 Too Many Requests` with a `Retry-After` header (seconds until the next token) and `code: RATE_LIMITED`. `/healthz` and `/readyz` are never throttled. Unauthenticated and per-IP limiting is the reverse proxy's job (see [Deployment](deployment.md)). Full model: [Quotas & rate limits](quotas.md).

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

Common codes: `INVALID_URL_SCHEME`, `INVALID_URL_FORMAT`, `SSRF_BLOCKED`, `INVALID_INTERVAL`, `INVALID_TIMEOUT`, `INVALID_TCP_PORT`, `INVALID_TCP_HOST`, `INVALID_STATUS_RANGE`, `INVALID_TLS_CERT_PARAMS`, `INVALID_DOMAIN_PARAMS`, `INVALID_TLS_CRED_COMBO`, `INVALID_ALERT_CONFIG`, `REDACTION_SENTINEL`, `BULK_EMPTY`, `BULK_TOO_LARGE`, `BAD_TIME_RANGE`, `TARGET_NOT_FOUND`, `CHANNEL_NOT_FOUND`, `CHANNEL_NAME_TAKEN`, `CHANNEL_NAME_INVALID`, `CHANNEL_QUOTA_EXCEEDED`, `INVALID_CHANNEL_CONFIG`, `CHANNEL_TEST_FAILED`, `CIRCUIT_OPEN`, `DEPENDENCY_DOWN`, `INTERNAL`.

### Quota, rate-limit and abuse codes

| Code | HTTP | Meaning |
|---|---|---|
| `QUOTA_EXCEEDED` | 422 | A plan quota would be exceeded. `details` carries `quota` (e.g. `max_targets`, `max_members`, `max_public_components`), `current`, `limit`, `plan`. |
| `MIN_CHECK_INTERVAL` | 422 | Requested check interval is below the effective floor (`max(plan.min_check_interval_secs, kind_min)`), where `kind_min` is 3600 for `tls_cert` / `domain_expiry` and 10 for `http` / `tcp` / `dns`. Enforced on create, bulk, **and** PATCH. |
| `INVITATIONS_LIMIT` | 409 | The org is at its pending-invitation cap. |
| `RATE_LIMITED` | 429 | A per-minute rate budget was exceeded. `Retry-After` (seconds) is set; `details.scope` names the tier, e.g. `per_org_api_writes`. |
| `ABUSE_BLOCKED` | 400 | Target blocked by abuse protection. `details.reason` explains. |
| `URL_PATTERN_BLOCKED` | 400 | Target URL matched an abuse pattern (recon path). |
| `DOMAIN_DENYLISTED` | 400 | Target domain (or a parent) is on the deny-list. |

See [Quotas & rate limits](quotas.md) for the quota model, the per-minute categories, and the deny-list policy.

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

## Latency series

`GET /api/v1/targets/{id}/latency?from=…&to=…`

Pre-bucketed quantiles and per-phase means read straight from the per-minute rollup — powers the monitor-detail latency line and phase-breakdown area charts. The server divides the range into ~60 slices (floored to the 60-second rollup grain), so any range returns a comparably dense series and the cost stays O(buckets), not O(samples). Switching range re-scales the buckets.

- `from` / `to` default to the last 24 h; `to` must be strictly greater than `from` (400 `BAD_TIME_RANGE`).

```jsonc
{
  "bucket_seconds": 1440,
  "buckets": [
    {
      "t": 1747137600000,      // unix-ms at bucket start (JS new Date(t))
      "p50": 120, "p95": 180, "p99": 240,
      "avg": 130,              // mean total; breakdown chart derives "processing" = avg − (dns+connect+tls+ttfb)
      "dns": 12, "connect": 20, "tls": 35, "ttfb": 60,  // mean per-phase ms; 0 for kinds that skip the phase
      "samples": 24            // 0 marks a gap the chart leaves unconnected
    }
  ]
}
```

`bucket_seconds` is always a multiple of 60 (1h→60, 24h→1440, 7d→10080, 30d→43200).

### Region filter

`results`, `latency`, and `uptime` accept an optional `region=` query parameter to scope the read to one probe region; omit it for an all-regions view. Region ids are the slugs registered via the operator surface. See [Multi-region probes](multi-region.md).

### Per-region latency series

`GET /api/v1/targets/{id}/latency/by-region?from=…&to=…`

Same bucketing and cost as `/latency`, but split by region so each can be overlaid as its own line — powers the monitor-detail overlay chart. One entry per region that has samples in the range; each region's `buckets` use the same shape as `/latency`.

```jsonc
{
  "bucket_seconds": 1440,
  "regions": [
    { "region": "default",  "buckets": [ /* LatencyBucket… */ ] },
    { "region": "eu-west",  "buckets": [ /* LatencyBucket… */ ] }
  ]
}
```

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

Returns every tag currently in use across the caller's targets (enabled or disabled), with target count, sorted by descending count then alphabetical. `q` is a prefix filter for autocomplete. Scoped to the active org — in SaaS mode another org's tags are invisible.

```jsonc
{ "items": [ { "name": "prod", "count": 12 }, { "name": "staging", "count": 4 } ],
  "total": 2, "limit": 100, "offset": 0 }
```

## Dashboard summary

`GET /api/v1/dashboard/summary` — per-org rollup cached in-process for 5 seconds (keyed by `OrgId`, so two tenants never share an entry).

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
