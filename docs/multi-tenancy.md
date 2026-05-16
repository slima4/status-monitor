# Multi-tenancy

status-monitor supports two operational modes from a single binary:

| Mode | `tenancy.enabled` | When to use |
|---|---|---|
| **Self-host** | `false` (default) | One operator (or one team) running their own monitoring. Every row belongs to a single auto-provisioned "default" org; the user never sees the concept. |
| **SaaS** | `true` | Multi-tenant deployment where users sign up, create orgs, and only see their own data. |

The two modes share the same code paths. Self-host is SaaS-with-one-org plus a session shortcut.

## The org model

Three tables form the access-control core:

```
organizations ── memberships ── users
                     │
                     └── role: 'owner' | 'member'
```

Every tenant-scoped table (`targets`, `incidents`, `incident_updates`, `maintenance_windows`, `maintenance_window_components`, …) carries `org_id NOT NULL` and an `ON DELETE CASCADE` foreign key to `organizations`. ClickHouse `check_results` and `check_results_1m` are partitioned by `(org_id, target_id, ts)` so single-org queries never full-scan the table.

### Slugs

Org slugs are case-insensitive (`CITEXT`), 3–30 characters, must start with a lowercase letter, and otherwise contain `[a-z0-9-]` only — no leading or trailing hyphen and no consecutive hyphens. A static reserved list (`api`, `admin`, `login`, …) is rejected at creation.

Auto-generated **personal-org slugs** take the shape `personal-{adj}-{noun}-{6char}` from inline word lists in `src/domain/word_lists.rs`. `create_org_with_owner` returns `Ok(None)` on a slug collision so callers wrap the generate-and-insert pair in a 5-attempt retry loop; the birthday-paradox tail above 5 retries is astronomically small. The current binary exposes the helper but the wrapping signup transaction lands with the auth backend.

### Three-org owner limit

A user can be `owner` of at most `free_tier_owner_org_limit` (default 3) **active** organisations. Enforced in a single SQL statement that puts the count subquery inside the `INSERT … WHERE …` so two concurrent creates cannot both win. Soft-deleted orgs do not count against the cap. Invited memberships (role `member`) are unlimited.

## Soft delete and the 30-day purge

Deletion is two-phase to give operators a recovery window and to keep ClickHouse rows out of forever-orphan state.

1. **Soft delete.** `DELETE /api/v1/orgs/{id}` flips `organizations.deleted_at = now()`. The org disappears from the user's switcher and every URL referencing it returns 404 — `is_active_member` short-circuits on `deleted_at IS NULL`.
2. **Restore window.** The original deleter can call `POST /api/v1/orgs/{id}/restore` within `deletion_grace_period_days` (default 30); the slug stays held to prevent squatting during this window.
3. **Purge.** A daily job (`src/jobs/retention.rs`) runs at 03:00 UTC. It first runs the soft-delete purge (`src/jobs/purge_deleted.rs::purge_tick`):
   - Selects up to 10 orgs whose `deleted_at` is past the grace window.
   - **Per org, in one PG transaction:** insert into `clickhouse_purge_queue` (idempotent via `ON CONFLICT (org_id) DO NOTHING`), then `DELETE FROM organizations` — `ON DELETE CASCADE` empties every tenant table.
   - Drains pending queue rows by issuing `ALTER TABLE check_results DELETE WHERE org_id = ?` against ClickHouse for each. The mutation is idempotent; a process restart between halves replays cleanly.
   - Then hard-deletes up to 10 soft-deleted users past the grace window that hold no live (unexpired, unused) recovery token. The `users` `ON DELETE CASCADE` erases memberships, oauth_identities, api_tokens, invitations, sessions and recovery tokens; rows referencing the user as an actor (`login_attempts`, `org_audit_log`, `quota_events`, `plan_overrides`) are kept with the actor nulled.

The same daily job then enforces long-horizon data retention from the `[retention]` config: it deletes `login_attempts`, `quota_events` and `org_audit_log` rows past their windows and reaps sessions that are absolute-expired **or** idle past `auth.session.idle_timeout_days`. ClickHouse `check_results` retention is the table's own `TTL` (background merge), kept equal to `retention.check_results_days`. Short-cadence security sweeps (OAuth-state, magic-link) keep their own faster loops — their frequency is the property.

The outbox table is the load-bearing piece. A naive "DELETE in PG, then DELETE in CH" sequence leaves CH rows orphaned if the worker dies between calls — invisible to queries but on disk forever, breaking the "data fully erased within 30 days" privacy claim.

## Per-org caches

`AppState` keeps tenant-derived caches keyed by `OrgId` so one tenant's data cannot leak into another's response:

| Cache | Type | TTL |
|---|---|---|
| `dashboard_cache` | `moka::sync::Cache<OrgId, Arc<DashboardSummary>>` | 5 s |
| `public_status::cache::PageCache` | `moka::future::Cache<OrgId, Arc<PageData>>` | 10 s |
| `PageCache::last_good` | `DashMap<OrgId, Arc<ArcSwap<PageData>>>` | retained across rebuilds for stale-fallback |

`PageCache::get_or_compute` does per-org single-flight via `moka`'s `try_get_with`, so a thundering herd against one org's page doesn't fan out into N expensive aggregator builds.

## Public status routes gating

> **Operator warning.** Until per-org status routing lands, the public status page (`/api/public/v1/status`, `/api/public/v1/badge.svg`, `/api/public/v1/incidents.rss`, `/status`, `/status/incidents/{id}`) is a single-aggregate view. Flipping `tenancy.enabled = true` while leaving these routes mounted would leak every tenant's public components to anonymous visitors.

The gate: when `tenancy.enabled = true`, the public-status routes only respond if `tenancy.public_routes_enabled = true` as well. Self-host mode (`tenancy.enabled = false`) ignores the flag — there is only one org, so there is nothing to leak. See [`public_routes_enabled` — the SaaS-mode gotcha](configuration.md#public_routes_enabled--the-saas-mode-gotcha) for the full mode/flag matrix.

## Tenant-isolation invariants

These are checked in CI:

- Every runtime SQL statement against a tenant table must include `org_id` in its `WHERE` clause. Enforced by `scripts/check_tenant_isolation.sh` via an `ast-grep` rule. The only allow-listed call sites are `src/storage/admin.rs` (`AdminRepo`, cross-tenant by design) and `src/storage/orgs.rs` (operates on the `organizations` table itself), plus `src/jobs/purge_deleted.rs` (drains soft-deleted orgs and users across tenants).
- Every ClickHouse `SELECT … WHERE target_id = …` must have a sibling `org_id = ?` term. Enforced by `scripts/check_clickhouse_org_scope.sh`.
- A Postgres trigger on every child table (`incident_updates`, `maintenance_window_components`) raises on `org_id` mismatch between child and parent rows.
- An integration test (`tests/tenant_isolation_test.rs`) provisions two orgs and asserts every per-org store backed by Postgres or ClickHouse only sees its own org's rows.

If you add a new tenant-scoped table or a new repository, make sure both ast-grep rules cover it before merge.

## Org-management API

See [REST API](api.md) for full schemas. The catalogue:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/v1/orgs` | Create org (slug, name) — caller becomes owner |
| `GET` | `/api/v1/orgs` | List orgs the caller is a member of |
| `GET` | `/api/v1/orgs/{id}` | Get one org (member-only) |
| `PATCH` | `/api/v1/orgs/{id}` | Edit org (owner-only) |
| `DELETE` | `/api/v1/orgs/{id}` | Soft-delete (owner-only) |
| `POST` | `/api/v1/orgs/{id}/restore` | Restore within the grace window (only by the deleter) |
| `GET` | `/api/v1/orgs/check-slug?slug=…` | Slug availability for signup forms |
| `GET` | `/api/v1/orgs/{id}/members` | List members (owner-only) |
| `DELETE` | `/api/v1/orgs/{id}/members/{user_id}` | Remove a member (owner-only) |
| `POST` | `/api/v1/me/active-org` | Switch the session's active org |
| `GET` | `/api/v1/me/orgs` | Active (non-deleted) orgs |
| `GET` | `/api/v1/me/deleted-orgs` | Soft-deleted orgs you deleted (restore UI) |
