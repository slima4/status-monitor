# Per-org status pages

In SaaS mode every org gets its own public status page at
`{slug}.status.{base_domain}` — `acme.status.example.com`,
`globex.status.example.com`, and so on. Each page renders **only** that
org's published components, incidents, and maintenance, with that org's
branding. It is opt-in: a new org's page is off until the owner turns it
on.

This chapter is the per-org model. For the component/incident/maintenance
workflow (identical per org) see [Public status page](public-status.md).
For the wildcard cert and reverse-proxy setup see
[Deployment](deployment.md#public-status-surface) and the full runbook in
[`deployment/README.md`](https://github.com/slima4/status-monitor/tree/main/deployment).

## When it applies

| Mode | Config | Public surface |
|---|---|---|
| Self-host | `tenancy.enabled = false` (default) | one org, served path-based at `/status` on the operator host |
| SaaS | `tenancy.enabled = true` + `tenancy.subdomain_public_routes = true` | one page per org at `{slug}.status.{base_domain}` |

Self-host never pays the subdomain path: there is a single org, so the
page is mounted on the operator host and `public_status_enabled` is set on
the provisioned org at startup. The rest of this chapter is SaaS.

Path-based public routes and subdomain public routes are mutually
exclusive in SaaS — serving `/status` on the operator host there would
publish one org's data to every tenant, so the binary refuses to boot
with both `tenancy.enabled = true` and `tenancy.path_based_public_routes
= true`. Use subdomains instead.

## Host routing

The org is resolved from the request `Host` header, not the path:

| Host | Result |
|---|---|
| `acme.status.example.com`, org enabled | that org's page |
| `acme.status.example.com`, org disabled or soft-deleted | **404** |
| `nope.status.example.com`, no such slug | **404** |
| `a.b.status.example.com` (extra label) | **404** |
| `status.example.com` (no slug label) | **404** |
| missing `Host` header | **404** |

`base_domain` must be a multi-label domain (it needs at least one dot);
the boot assertion refuses an empty or single-label value, because a
loose base would let the slug extractor match arbitrary `Host` headers.

The wildcard `*.status.{base_domain}` DNS record plus a wildcard TLS cert
(Let's Encrypt via the Hetzner DNS-01 challenge) means a new org's page
works the instant the owner enables it — no per-org DNS or cert step.

## Enabling and branding an org's page

The org **owner** controls this from the operator UI at
`/settings/status-page`, or over the API:

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/orgs/{id}/status-page` | current settings + the live page URL |
| `PATCH /api/v1/orgs/{id}/status-page` | toggle on/off and edit branding |
| `POST /api/v1/orgs/{id}/status-page/logo` | upload a logo (multipart) |
| `DELETE /api/v1/orgs/{id}/status-page/logo` | remove the logo |

Only an owner of **that** org may call these — membership is checked
against the org id in the path, not the caller's active org, so an owner
of one org can't edit another's page.

Branding fields:

| Field | Rule | Default when unset |
|---|---|---|
| `public_status_enabled` | the on/off switch | off |
| `public_display_name` | 1–80 chars | the org's name |
| `public_brand_color` | `#RRGGBB` (6-digit hex) | `#3b82f6` |
| `public_about` | Markdown, ≤ 500 chars, rendered to sanitised HTML | omitted |
| logo | PNG / JPEG / WebP, ≤ 1 MB, ≤ 1200 px; larger images are downscaled. Format is sniffed from the bytes (declared content-type ignored — a script/SVG can't masquerade as an image) and the decoder is allocation- and dimension-bounded against decompression bombs | header shows the display name as text |
| `public_show_powered_by` | footer attribution toggle | on |

The settings page shows a live link to the page so the owner can preview
exactly what visitors see.

### About text

`public_about` is Markdown. It is parsed and then run through an HTML
sanitiser before it ever reaches a template: only `p`, `strong`, `em`,
`a`, `br`, `ul`, `ol`, `li` survive, links get
`rel="noopener nofollow"`, and there is no raw-HTML escape hatch. Scripts
and inline styles are stripped.

### Brand colour

The colour is validated at three independent layers — the database
constraint, the application validator, and again in the template right
before it is written into the page's `<style>`. Any value that isn't a
strict 6-digit hex falls back to the default at render time, so a relaxed
constraint at one layer can't open a CSS-injection path on its own.

### Logo storage

An uploaded image's format is detected from its **bytes**, not its
declared content type. The on-disk filename is derived from the org id
and a hash of the content, never from anything the client sends, so a
crafted filename can't escape `public_status.logo_dir`. Replacing or
removing a logo deletes the previous file.

## Caching and turning a page off

Each org's rendered page is cached for `public_status.cache_ttl_secs`
(default 10 s). A separate last-known-good layer keeps the most recent
successful render per org so a transient Postgres/ClickHouse blip serves
slightly stale data instead of an error. That layer is bounded by
`cache_max_orgs` and idle-evicts after `last_good_ttl_secs`, so churn
through many orgs can't grow it without limit.

Turning a page off (`public_status_enabled` → false) drops both cache
layers for that org immediately, and the host resolver stops resolving
the slug, so the page is a 404 within one TTL window at most. Deleting an
org has the same effect via the purge worker.

## Security model

- **Opt-in only.** The public host resolver uses a lookup that filters
  `public_status_enabled = true AND deleted_at IS NULL`. A disabled or
  deleted org's slug resolves to 404 even though the slug still exists.
  The authenticated org lookup is a separate function and is never used
  on the public path.
- **Operator sessions never reach status subdomains.** The session
  cookie is host-only (`auth.session.cookie_domain = ""`), so the browser
  scopes it to the operator host and never sends it to
  `*.status.{base_domain}`. The binary refuses to boot if
  `cookie_domain` is set to a parent zone that would overlap the status
  wildcard.
- **No operator surface on the page.** The status page renders no
  operator UI, sets no cookies, and never echoes request auth headers.
- **Tenant isolation.** A request for org B's host returns only org B's
  data; the page cache and data sources are keyed by org id end to end.

## Configuration

The `[public_status]` block and the split tenancy flags are documented in
[Configuration → Public status page](configuration.md#public-status-page)
and [Configuration → Multi-tenancy mode](configuration.md#multi-tenancy-mode).

## Coming in v1.1: custom domains

v1 serves every org under the shared `*.status.{base_domain}` wildcard.
v1.1 will let an org point its own hostname (e.g.
`status.theirbrand.com`) at the service:

- the org adds a `CNAME` to `{slug}.status.{base_domain}` and registers
  the custom hostname on its settings page;
- the reverse proxy issues a per-hostname certificate on demand (no
  wildcard for custom domains — each is a distinct name);
- host resolution gains a custom-domain → org lookup ahead of the
  subdomain parser; everything downstream (cache, branding, isolation) is
  unchanged.

This is intentionally additive: the subdomain path keeps working as the
always-available default, and nothing in the v1 data model blocks it.
Custom domains are **not** available in v1 — track the roadmap before
promising a customer a vanity status URL.
