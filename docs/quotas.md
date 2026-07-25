# Quotas & rate limits

Every organization is bound to a **plan**. The plan is the single source of
truth for resource quotas and per-minute rate budgets — the number a request
is enforced at is the same number the API reports back. A new tier is one row
in the `plans` table; nothing in the enforcement path changes.

## The seeded plans

Three plans ship seeded: `free`, `founding` (a more generous
free tier granted to early accounts and kept for life), and `pro`. Only
`free` is listed; the other two are assigned by signup or billing. On the
hosted service `free` is sold as **Standard**, `founding` as **Founding**, and
`pro` as **Pro**; see the [pricing page](https://uptimepage.dev/pricing).

| Quota | free | founding | pro | Meaning |
|---|---|---|---|---|
| `max_targets` | 20 | 50 | 150 | Monitored targets in the org |
| `min_check_interval_secs` | 180 | 60 | 30 | Plan-side floor on a target's check interval. The effective floor is `max(this, kind_min)` — `kind_min` is 3600 for `tls_cert` / `domain_expiry`, 300 for `flow`, 60 for `heartbeat`, and 10 for `http` / `tcp` / `dns` / `ping`. |
| `retention_days` | 30 | 90 | 395 | History window the UI and API will read |
| `raw_days` | 30 | 30 | 30 | Per-check detail retention, stamped onto each ClickHouse row at write time |
| `max_flow_checks` | 0 | 0 | 0 | Browser flow monitors the org can create; 0 doubles as the feature gate, and every plan stays there until launch sets real caps |
| `max_regions` | 3 | ∞ | ∞ | Regions a single monitor can be assigned to |
| `max_members` | 3 | 5 | 15 | Active members in the org |
| `max_pending_invitations` | 10 | 15 | 25 | Outstanding (unaccepted) invitations |
| `max_api_tokens_per_user` | 5 | 7 | 10 | API tokens a single user may hold |
| `max_status_pages` | 1 | 2 | 5 | Public status pages the org can run |
| `max_public_components` | 15 | 30 | 75 | Distinct monitors published across all of the org's pages (a monitor on several pages counts once) |
| `max_share_links_per_monitor` | 1 | 3 | 5 | Live share links on one monitor |
| `max_shared_monitors` | 2 | 5 | 10 | Monitors with at least one share link |
| `max_maintenance_windows` | 20 | 30 | 50 | Scheduled maintenance windows |
| `max_notification_channels` | 20 | 30 | 50 | Notification channels (Slack/webhook/Telegram/WhatsApp/SMS/…) in the org |
| `max_escalation_policies` | 10 | 10 | 50 | Escalation policies |
| `max_on_call_schedules` | 5 | 5 | 25 | On-call schedules |
| `max_logo_size_bytes` | 1048576 | 1048576 | 1048576 | Status-page logo upload ceiling (1 MiB) |

Feature flags ride on the same row: `custom_domain_enabled`, `white_label_enabled`,
`sms_alerts_enabled`, `incident_narration_enabled`, `on_call_enabled`.
`white_label_enabled` is what makes the status-page "powered by" toggle real:
on a plan without it the badge always renders, whatever the page setting says.

| Rate budget (per minute) | free | founding | pro | Category |
|---|---|---|---|---|
| `api_writes_per_minute` | 600 | 900 | 1200 | POST/PATCH/DELETE on `/api/v1/*` |
| `api_reads_per_minute` | 6000 | 9000 | 12000 | GET/HEAD/OPTIONS on `/api/v1/*` |
| `bulk_ops_per_minute` | 30 | 45 | 60 | `/api/v1/targets/bulk*` |
| `test_now_per_minute` | 60 | 90 | 120 | `POST /api/v1/targets/test` + the notification-channel test endpoints |
| `check_now_per_minute` | 60 | 90 | 120 | `POST /api/v1/targets/{id}/check-now` |

## How quotas are enforced

A resource quota is checked **atomically at the write**, not by a
check-then-act in the handler. The friendly handler-side pre-check exists
only to produce a clean error on the common, uncontended path; the race-safe
guarantee is in the store:

- **Targets** — the count bound is inside the `INSERT` (single and bulk),
  handed the same `max_targets`. Concurrent creates at `limit - 1` settle at
  exactly `limit`, never more.
- **Members** — the membership insert runs under a per-org advisory lock,
  counts, and rolls itself back if it crossed `max_members`. Re-adding an
  existing member stays a no-op.
- **Pending invitations** — dedupe and the pending cap are enforced in one
  transaction under the same per-org lock; parallel duplicate-email invites
  yield exactly one row.
- **Public components** — the cap is enforced when a monitor is added as a
  status-page component, counting distinct monitors across all of the org's
  pages in the same transaction as the insert.
- **API tokens** — count-in-`INSERT`, scoped per user, handed
  `max_api_tokens_per_user`.

Exceeding a resource quota returns **422**:

```jsonc
{
  "error": {
    "code": "QUOTA_EXCEEDED",
    "message": "max_targets limit reached: 20 of 20 used on the free plan.",
    "field": null,
    "details": { "quota": "max_targets", "current": 20, "limit": 20, "plan": "free" },
    "trace_id": null
  }
}
```

The pending-invitation cap is the one exception to the code: it predates the
unified envelope and returns **409 `INVITATIONS_LIMIT`**. The cap itself is
enforced identically (atomic, never overshoot).

A sub-minimum check interval is its own 422, `MIN_CHECK_INTERVAL`, enforced
on create and PATCH, single and bulk — a target created at the floor cannot
be edited below it. The floor is `max(plan.min_check_interval_secs, kind_min)`:
the per-kind value (3600 for `tls_cert` / `domain_expiry`, 300 for `flow`,
60 for `heartbeat`, 10 for the rest) applies regardless of plan tier —
polling an expiry probe faster than once an hour yields no signal.

## Rate limiting

Two app-side tiers, both keyed on the **authenticated subject** (never the
TCP peer): `(org, category)` and `(user, category)`. Both are checked; the
org tier fires first because it protects shared resources. The per-minute
budget comes from the org's plan. The request category is derived from the
path and method:

- path contains `/bulk` → `bulk_ops`
- path ends `/test` → `test_now`
- path ends `/check-now` → `check_now`
- any path under `/mcp` → `api_reads`, whatever the method (the JSON-RPC body hides the tool name from the middleware; probe-spawning and write tools re-check the stricter category inside the tool)
- otherwise `GET`/`HEAD`/`OPTIONS` → `api_reads`, else → `api_writes`

Exceeding a budget returns **429** with a `Retry-After` header:

```jsonc
{
  "error": {
    "code": "RATE_LIMITED",
    "message": "Too many requests.",
    "field": null,
    "details": { "scope": "per_org_api_writes", "retry_after_secs": 30 },
    "trace_id": null
  }
}
```

The limiter is a `governor` cell per `(scope, category)` key in a `DashMap`.
A janitor evicts entries idle past the threshold so the map stays bounded by
the number of *active* tenants, not by request volume; its lifetime is bound
to the limiter so a refactor cannot silently drop the sweep and leak the
map. Unauthenticated requests fall through untouched — per-IP limiting for
those (auth endpoints, org creation, the public status surface) is the
reverse proxy's job; see [Deployment](deployment.md).

Checks themselves are **not** rate-limited — the scheduler path never enters
this middleware, so monitoring throughput is unaffected.

Every quota / rate-limit / abuse rejection is recorded to the append-only
`quota_events` table (`event`, `quota_name`, `details`, hashed IP) as
fire-and-forget — it never blocks the response. It is the data source for
abuse review.

## Usage transparency

| Endpoint | Returns |
|---|---|
| `GET /api/v1/orgs/{id}/usage` | Plan + current vs limit for every org-scoped quota, policy values, rate budgets, feature flags. Member-gated (a non-member gets the same 404 as `GET /orgs/{id}`). |
| `GET /api/v1/me/usage` | The caller's `api_tokens` and `owned_orgs` current/limit. |

The operator UI surfaces the same numbers at `/settings/usage` as progress
bars (an unlimited self-host limit renders as ∞). Reported limit == enforced
limit by construction: both read the same plan and the same count query.

## Anti-abuse

Two deny-lists, applied when a target is created, bulk-created, updated, or
test-run. A block is a **400**, audited to `quota_events` with
`event = abuse_blocked`.

- **URL patterns** — a case-insensitive regex set of attack-recon paths
  (exposed VCS dirs, `.env`, credential paths, admin panels, WordPress
  `xmlrpc` pingback, Spring actuator, backup/dump extensions, …). A match
  is `400 URL_PATTERN_BLOCKED` / `ABUSE_BLOCKED`. The shipped patterns and
  the compiled fallback are kept byte-identical by a drift guard.
- **Domains** — a YAML deny-list (`config/abuse_denylist.yaml`) matched
  **hierarchically**: listing `example.com` also blocks
  `eu.status.example.com`. It carries the operator's own domain (don't
  monitor yourself) and competing uptime/status providers (monitoring
  another monitor forms a load-amplification chain). A match is
  `400 DOMAIN_DENYLISTED`. Dedicated monitoring SaaS are listed at the apex;
  multi-tenant status-page hosts are listed narrowly so legitimate
  vendor-status checks are not over-blocked.

The lists load at startup. With `abuse.hot_reload_enabled` set, sending
SIGHUP re-reads and validates them and swaps them in atomically; a bad edit
keeps the old rules. Without it, changes need a restart. A bad regex or
malformed YAML at startup is a clean *config error*, never a crash loop.

## Configuration

```toml
[quotas]
plan_cache_ttl_secs  = 300   # org→plan cache; a plans-table edit takes
usage_cache_ttl_secs = 10    #   effect within this window
```

A plans-table change is invisible until the plan cache's TTL elapses (a
cache hit is zero DB round-trips on the hot path), then the next lookup
refetches.

Single-tenant deploys raise limits the same way SaaS does: edit (or
INSERT) the `plans` row the org is assigned to, or attach a
`plan_overrides` row with the cap fields you want to raise. There is no
config-side override knob — every quota lives in Postgres so the
audit-trail covers both modes.

Every numeric quota / rate / interval is validated at config load —
`< 1` is rejected with the offending field named, never a panic in
router or limiter construction.

The reverse-proxy per-IP tiers (auth endpoints, org creation, public
surface) are documented in [Deployment](deployment.md).
