# Quotas & rate limits

Every organization belongs to an **account**, and the account is bound to a
**plan**. The plan is the single source of truth for resource quotas and
per-minute rate budgets — the number a request is enforced at is the same
number the API reports back. A new tier is one row in the `plans` table;
nothing in the enforcement path changes.

The account is the quota subject, not the org. Every count below spans all of
the account's live orgs, so a second org is a workspace over the same pool, not
a second allowance — and a member invited into someone else's org brings access
only, never capacity. `max_orgs` bounds how many workspaces the pool is split
across. Soft-deleted orgs are outside the pool while they wait out the deletion
grace window (their monitoring is paused), which is why restoring one re-checks
`max_orgs` the same way creating one does.

## The seeded plans

Three plans ship seeded: `free`, `founding` (a more generous
free tier granted to early accounts and kept for life), and `pro`. Only
`free` is listed; the other two are assigned by signup or billing. On the
hosted service `free` is sold as **Standard**, `founding` as **Founding**, and
`pro` as **Pro**; see the [pricing page](https://uptimepage.dev/pricing).

| Quota | free | founding | pro | Meaning |
|---|---|---|---|---|
| `max_orgs` | 1 | 3 | 5 | Organizations the account may hold. They share every other quota on this table |
| `max_targets` | 20 | 50 | 150 | Monitored targets across the account's orgs |
| `min_check_interval_secs` | 180 | 60 | 30 | Plan-side floor on a target's check interval. The effective floor is `max(this, kind_min)` — `kind_min` is 43200 for `domain_expiry`, 3600 for `tls_cert`, 300 for `flow`, 60 for `heartbeat`, and 10 for `http` / `tcp` / `dns` / `ping`. |
| `retention_days` | 30 | 90 | 395 | History window the UI and API will read |
| `raw_days` | 30 | 30 | 30 | Per-check detail retention, stamped onto each ClickHouse row at write time |
| `evidence_days` | 7 | 7 | 7 | How long a failed browser-flow run keeps the page it captured. Clamped to `raw_days`, since the run it explains goes then |
| `max_flow_steps` | 30 | 30 | 30 | Steps one flow monitor may declare. Clamped to the engine ceiling of 30, so a larger value has no effect |
| `max_flow_checks` | 0 | 0 | 0 | Browser flow monitors the org can create; 0 doubles as the feature gate, so a create returns `403 FLOW_CHECKS_DISABLED` rather than a quota error. Seeded at 0 because a flow also needs `flow.enabled` on the process that runs it; raise it on the plan row to switch flow on. The hosted service sets its own values, listed on [Plans and limits](hosted/plans-and-limits.md) |
| `max_regions` | 3 | ∞ | ∞ | Regions a single monitor can be assigned to |
| `max_members` | 3 | 5 | 15 | Distinct people across the account's orgs — one person in two orgs holds one seat |
| `max_pending_invitations` | 10 | 15 | 25 | Outstanding (unaccepted) invitations |
| `max_api_tokens_per_user` | 5 | 7 | 10 | API tokens a single user may hold |
| `max_status_pages` | 1 | 2 | 5 | Public status pages across the account's orgs |
| `max_public_components` | 15 | 30 | 75 | Distinct monitors published across all of the account's pages (a monitor on several pages counts once) |
| `max_share_links_per_monitor` | 1 | 3 | 5 | Live share links on one monitor |
| `max_shared_monitors` | 2 | 5 | 10 | Monitors with at least one share link |
| `max_maintenance_windows` | 20 | 30 | 50 | Scheduled maintenance windows |
| `max_notification_channels` | 20 | 30 | 50 | Notification channels (Slack/webhook/Telegram/WhatsApp/SMS/…) across the account's orgs |
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

One category sits outside the plan: `support` (`POST /api/v1/support`, the
in-app help form) is capped at a fixed 2 per minute on every tier. It spends the
operator's mail budget rather than a tenant resource, so paying more does not
buy a larger share of it.

## How quotas are enforced

A resource quota is checked **atomically at the write**, not by a
check-then-act in the handler. The friendly handler-side pre-check exists
only to produce a clean error on the common, uncontended path; the race-safe
guarantee is in the store:

Each guard takes a **per-account** advisory lock, because the count it protects
spans the account's orgs: two creates in two different orgs of one account have
to contend, not race.

- **Targets** — the count bound is inside the `INSERT` (single and bulk),
  handed the same `max_targets`. Concurrent creates at `limit - 1` settle at
  exactly `limit`, never more.
- **Members** — the membership insert runs under the account lock, counts
  distinct people, and rolls itself back if it crossed `max_members`. Re-adding
  an existing member stays a no-op, and so does adding someone who already
  holds a seat in a sibling org.
- **Pending invitations** — dedupe (per org) and the pending cap (per account)
  are enforced in one transaction under the account lock; parallel
  duplicate-email invites yield exactly one row.
- **Public components** — the cap is enforced when a monitor is added as a
  status-page component, counting distinct monitors across all of the account's
  pages in the same transaction as the insert.
- **Organizations** — `create` counts the account's live orgs under its lock
  and refuses past `max_orgs` with 422 `OWNER_ORG_LIMIT`; restore re-checks the
  same number.
- **API tokens** — count-in-`INSERT`, scoped per user, handed
  `max_api_tokens_per_user`. This one is genuinely per user, not per account.

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
the per-kind value (43200 for `domain_expiry`, 3600 for `tls_cert`, 300 for
`flow`, 60 for `heartbeat`, 10 for the rest) applies regardless of plan tier —
polling an expiry probe faster yields no signal, and `domain_expiry` reads
RDAP, which rate-limits by source address.

## Rate limiting

Two app-side tiers, both keyed on the **authenticated subject** (never the
TCP peer): `(account, category)` and `(user, category)`. Both are checked; the
account tier fires first because it protects shared resources. The per-minute
budget comes from the account's plan, except for `support`, which is fixed.
The first tier keys on the account for the same reason the resource caps pool:
one budget however many workspaces the customer splits their traffic across. The
request category is derived from the path and method:

- path contains `/bulk` → `bulk_ops`
- path ends `/test` → `test_now`
- path ends `/check-now` → `check_now`
- path ends `/support` → `support`
- any path under `/mcp` → `api_reads`, whatever the method (the JSON-RPC body hides the tool name from the middleware; probe-spawning and write tools re-check the stricter category inside the tool)
- otherwise `GET`/`HEAD`/`OPTIONS` → `api_reads`, else → `api_writes`

Exceeding a budget returns **429** with a `Retry-After` header:

```jsonc
{
  "error": {
    "code": "RATE_LIMITED",
    "message": "Too many requests.",
    "field": null,
    "details": { "scope": "per_account_api_writes", "retry_after_secs": 30 },
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
| `GET /api/v1/orgs/{id}/usage` | Plan + current vs limit for every pooled quota, policy values, rate budgets, feature flags. The counts are the account's totals across its live orgs, so they can exceed what the org in the path holds. Member-gated (a non-member gets the same 404 as `GET /orgs/{id}`). |
| `GET /api/v1/me/usage` | The caller's `api_tokens` (genuinely per user) and `owned_orgs` (the account's orgs against `max_orgs`), each current/limit. |

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
plan_cache_ttl_secs  = 300   # org→account→plan cache; a plans-table edit
usage_cache_ttl_secs = 10    #   takes effect within this window
default_plan         = "pro"   # plan the boot-seeded owner account is placed on
```

A plans-table change is invisible until the plan cache's TTL elapses (a
cache hit is zero DB round-trips on the hot path), then the next lookup
refetches.

`free` is priced for a shared platform: its ceilings bound what one tenant
can cost the host. On your own hardware there is no such cost, so a
self-hosted install needs no setup here — `default_plan` already defaults to
`pro`, giving the seeded owner account 150 monitors, five orgs to spend them
across, a 30s check floor and 13-month retention.

Only boot-time seeding reads it, so it applies to the account behind the owner
org that first run creates and to nothing else. The operator CLI
(`bootstrap-owner`) does not use it, and an account you re-plan later is never
moved back on the next boot. A second org that owner creates joins the same
account and shares its plan and pool; a *different* person signing up opens
their own account, which gets `founding` until that tier's cutoff and `free`
after it. Set `default_plan = "free"` to opt out entirely.

The plan id is resolved against the `plans` table before the seed writes
anything, so a typo fails the boot with the name quoted and leaves nothing
half-seeded behind.

Quota *values* still live only in Postgres — `default_plan` chooses a plan,
it does not override any number in one. Raise limits the way SaaS does: edit
(or INSERT) the `plans` row the account is assigned to, or attach a
`plan_overrides` row (keyed by account, `max_orgs` included) with the cap
fields you want to raise, so the audit trail covers both modes.

Every numeric quota / rate / interval is validated at config load —
`< 1` is rejected with the offending field named, never a panic in
router or limiter construction.

The reverse-proxy per-IP tiers (auth endpoints, org creation, public
surface) are documented in [Deployment](deployment.md).
