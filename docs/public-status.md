# Public status page

The public status page is the customer-facing surface — an unauthenticated
HTML page at `/status` plus a small JSON + RSS API under
`/api/public/v1/*`. It's the only part of uptimepage that's safe to
expose on the open internet without basic auth in front of it.

This chapter is for operators: how to publish a component, narrate an
incident, and schedule a maintenance window. For the wire-level details
of the underlying endpoints see [REST API](api.md#public-status-endpoints).
For Caddy + the rate-limit plugin see [Deployment](deployment.md#public-status-surface).

> **Multi-tenant operators read this first.** This chapter describes the
> page itself; the workflow is identical on every page. In a multi-tenant
> deployment each org runs one or more pages at `{slug}.{base_domain}` — set
> `tenancy.subdomain_public_routes = true` and leave
> `tenancy.path_based_public_routes` off. The path-based `/status` surface
> is single-org and is for single-tenant deploys only (the default). See
> [Per-org status pages](per-org-status.md) for the routing, branding,
> and isolation model, and
> [Public status routing](configuration.md#public-status-routing) for
> the flag matrix.

## What's published vs what's private

By default every target is **private**. A monitor becomes a "component"
on a status page only when it is curated onto that page — there is no
per-target "public" flag. The aggregator filters at the SQL layer (a
page renders only the monitors bound to it) and the wire types literally
cannot serialise sensitive fields (`url`, `headers`, `basic_auth`,
`bearer_token` are not part of any public schema), so a misconfiguration
cannot leak credentials.

A monitor is published by adding it to a page; the per-page presentation
lives on that binding, so the same monitor can appear on several pages
under different names:

| Per-page field | Purpose |
|---|---|
| (binding exists) | the monitor appears as a component on that page |
| `public_name` | display name on this page; falls back to the operator-side monitor name when unset |
| `public_description` | optional one-liner shown under the component name |
| `public_group` | optional group label; components with the same value cluster together. Ungrouped components render last |
| `sort_order` | integer sort key within a group (ASC); the reorder endpoint rewrites it |

A page belongs to an org and is managed by that org's owner; see
[Per-org status pages](per-org-status.md) for the page model, the
`max_status_pages` / `max_public_components` caps, and isolation.

## Enabling a component

The quickest path is the UI: open the page in **Settings → Pages →
{your page}**. The editor lists every monitor in the org; toggle one
**on page**, optionally set a **Public name** (blank shows the real
monitor name) and a **Group**. Each edit autosaves via the components
API below.

For scripting, add the monitor to the page, then set its per-page
curation:

```bash
# Add monitor $TARGET_ID to page $PAGE_ID
curl -X POST http://127.0.0.1:8080/api/v1/status-pages/$PAGE_ID/components \
  -H 'content-type: application/json' \
  -d '{"target_id": "'$TARGET_ID'", "public_name": "Public API", "public_group": "Core APIs"}'

# Edit the per-page name / description / group later
curl -X PATCH http://127.0.0.1:8080/api/v1/status-pages/$PAGE_ID/components/$TARGET_ID \
  -H 'content-type: application/json' \
  -d '{"public_description": "Primary REST surface, all regions."}'

# Remove it from the page
curl -X DELETE http://127.0.0.1:8080/api/v1/status-pages/$PAGE_ID/components/$TARGET_ID
```

On the `PATCH`, `public_name`, `public_description`, and `public_group`
use the same three-state semantics as incident narration: **omit** the
field to leave it unchanged, send a string to set it, or send JSON
`null` to clear it back to the default (real monitor name / no group).
Blanking the field in the UI clears it for you.

Adding a monitor that's already on the page is an idempotent no-op.
Adding a brand-new monitor when the org is at its `max_public_components`
cap is a quota error; a monitor already published on another page costs
nothing to add here.

The page is cached for 10 s in-process (moka single-flight, with a
second moka last-known-good cache so transient ClickHouse failures
don't break the page). Changes appear on the next refresh.

## Narrating an incident

The background incident writer opens an incident automatically when a
public target trips the threshold; it closes it again when checks
recover. Both events happen without operator action. What's manual is
the **narration** — the human-readable title, description, severity,
and the running timeline of "investigating → identified → monitoring →
resolved" entries that show up on `/status` and in the RSS feed.

Update the title + severity:

```bash
curl -X PATCH http://127.0.0.1:8080/api/v1/incidents/$INCIDENT_ID \
  -H 'content-type: application/json' \
  -d '{
    "public_title": "Elevated 5xx in EU-WEST",
    "public_description": "Origin rollout regression — rolling back.",
    "severity": "major"
  }'
```

Sending JSON `null` for `public_title` or `public_description` clears
the field and lets the page fall back to its auto-generated wording.
Omitting the field leaves it unchanged.

Append a status update to the timeline:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/incidents/$INCIDENT_ID/updates \
  -H 'content-type: application/json' \
  -d '{
    "phase": "identified",
    "message": "Rolled back the offending deploy. Verifying recovery."
  }'
```

`phase` is one of `investigating`, `identified`, `monitoring`,
`resolved`, `postmortem`. Posting `resolved` does **not** end the
incident — the incident lifecycle is driven by check results, so manual
"resolved" entries are advisory only. Posting an update to an
already-ended incident is allowed (useful for postmortems).

Validation rules:

| Field | Rule | Error code |
|---|---|---|
| `public_title` | non-whitespace, ≤ 200 chars (use JSON `null` to clear) | `EMPTY_TITLE` / `TITLE_TOO_LONG` |
| `public_description` | ≤ 5 000 chars (use `null` to clear) | `DESCRIPTION_TOO_LONG` |
| `message` (update) | non-whitespace, ≤ 2 000 chars | `EMPTY_MESSAGE` / `MESSAGE_TOO_LONG` |
| `phase` (update) | exactly one of the five values above | `400` / `422` from the JSON extractor |

## Scheduling maintenance

A maintenance window is a planned outage. While the window is active,
the page renders affected components as `Maintenance` (the truth-table
rule is: maintenance dominates outage, so a real failure during the
window still classifies as `Maintenance`, not `MajorOutage`). On the
90-day history strip, any day that overlapped a maintenance window
renders as a maintenance cell rather than an outage cell.

Create:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/maintenance \
  -H 'content-type: application/json' \
  -d '{
    "title": "PG13 → PG16 cutover",
    "description": "Read-only for ~30 minutes.",
    "starts_at": "2026-05-14T22:00:00Z",
    "ends_at":   "2026-05-14T23:00:00Z",
    "component_ids": ["01a7b1ce-0000-7000-8000-000000000001"]
  }'
```

List, edit, delete:

```bash
curl 'http://127.0.0.1:8080/api/v1/maintenance?status=upcoming&limit=10'
curl -X PATCH http://127.0.0.1:8080/api/v1/maintenance/$ID \
     -H 'content-type: application/json' \
     -d '{"title": "PG cutover (postponed)"}'
curl -X DELETE http://127.0.0.1:8080/api/v1/maintenance/$ID
```

Validation rules:

| Field | Rule | Error code |
|---|---|---|
| `title` | non-whitespace, ≤ 200 chars | `EMPTY_TITLE` / `TITLE_TOO_LONG` |
| `description` | ≤ 5 000 chars | `DESCRIPTION_TOO_LONG` |
| `ends_at` | strictly after `starts_at` | `INVALID_TIME_RANGE` |
| `ends_at - starts_at` | ≤ 30 days | `INVALID_DURATION` |
| `component_ids` | every id must reference an existing target | `INVALID_COMPONENT_ID` |
| PATCH on a window whose `ends_at` is already past | rejected | `422 MAINTENANCE_COMPLETED` |

For audit, prefer PATCHing a cancelled window's title (e.g. `"[cancelled]
PG cutover"`) over hard-deleting historical entries.

## What the public page renders

- **Banner** — one of `All Systems Operational`, `Maintenance in
  progress`, `Minor Service Disruption`, `Partial System Outage`,
  `Major System Outage`. Driven by the worst component state, with
  maintenance precedence as described above.
- **Component groups** — each component shows its current state, a
  90-day history strip (one cell per day, oldest-first), and the
  operator-supplied description.
- **Active and recent incidents** — operator-set `public_title` if
  present, otherwise an auto-generated `"<component> <status>"`
  string. Each incident links to a permalink at
  `/status/incidents/{id}` with the full timeline.
- **Maintenance** — active + the next 7 days of upcoming windows.
- **RSS feed** — `/api/public/v1/incidents.rss`. RSS 2.0; each item is
  a public incident with the latest update as the description.

## Refresh behaviour

The page is statically rendered and works without JavaScript. With JS
enabled, an HTMX `hx-trigger="every 30s"` swaps the dynamic region (the
banner, the component grid, and the incident lists) without a full
page reload. The chrome around it — header, footer, RSS link — stays
put. A small (~35 LoC) `static/js/public/tz.js` helper rewrites
ISO timestamps into the visitor's local timezone tooltip; everything
else is plain HTML.

## Caddy and the rate-limit plugin

The public surface bypasses basic auth at the Caddy layer through an
`@public` matcher in `deployment/Caddyfile`. The matcher also applies a
per-IP rate limit (60 requests / minute), which requires the
[`caddy-ratelimit`](https://github.com/mholt/caddy-ratelimit) plugin.
The stock `caddy:2-alpine` image doesn't include it — build a
`custom-caddy:2` image once via `xcaddy`. The procedure is in
[Deployment](deployment.md#public-status-surface) and
[`deployment/README.md`](https://github.com/uptimepage/uptimepage/tree/main/deployment).

If you'd rather not maintain a custom Caddy image, comment out the
`rate_limit { … }` block in the Caddyfile. The public surface still
serves; you just lose per-IP throttling. Putting Cloudflare in front of
Caddy is the other option.

## Embeddable status badge

`GET /api/public/v1/badge.svg` returns a shields.io-style SVG badge that
operators can embed in README files or external dashboards. Two modes:

```markdown
<!-- Overall page status -->
![status](https://status.example.com/api/public/v1/badge.svg)

<!-- Single component -->
![api status](https://status.example.com/api/public/v1/badge.svg?component=<uuid>)
```

The badge reuses the cached page payload, so it tracks the `/status`
view inside the 10-second cache window. Unknown component ids return
`404` with the public error envelope; only `style=flat` is recognised
(others return `400`).

## Common questions

**Can I have a component that's public but doesn't trigger incidents?**
No. Incident materialisation walks the same binding the page does — a
monitor on any enabled page is eligible for incidents. If you want a
check that's published but not alerting, set `enabled = false` on the
alert channels — the incident will still open, but no notification
fires.

**Can I publish a maintenance window without listing the affected
components?** No. `component_ids` may be empty in the request body, but
the aggregator filters maintenance windows that touch zero public
components out of the page (and out of the JSON), so they wouldn't
appear anywhere. List at least one public component.

**What's the cache TTL?** 10 s. Single-flight: only one task computes
the page when the entry expires; others wait for the result. On
ClickHouse failure the last-known-good snapshot serves until the next
successful compute.

**How long does the 90-day history go back?** Exactly 90 days, oldest
day on the left. Cells with no recorded checks render as `NoData`
(grey); the aggregator does not fabricate data.

**Is there an Atom feed?** No, RSS 2.0 only. Most feed readers consume
both.
