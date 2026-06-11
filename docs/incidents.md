# Incident management

uptimepage turns a failing check into a first-class operational incident: a tracked lifecycle with acknowledgement, ownership, paging, on-call rotations, escalation, and a retrospective — not just a banner on a status page. This chapter is for operators running incident response. For the customer-facing surface it publishes to, see [Public status page](public-status.md); for the wire-level endpoints see [REST API](api.md).

## The core idea: internal state is not public phase

The single most important distinction is that what your responders see is orthogonal to what your customers see. Conflating the two is the classic incident-tooling bug, so uptimepage keeps three independent axes on one incident:

| Axis | Values | Audience | Changed by |
|---|---|---|---|
| **Internal state** | `triggered` → `acknowledged` → `resolved` | Responders | Acknowledge / resolve / reopen actions |
| **Public phase** | `investigating` / `identified` / `monitoring` / `resolved` / `postmortem` | Customers on a status page | Operator-posted public updates only |
| **Visibility** | `internal` / `public` | — | An explicit publish action |

Acknowledging an incident stops escalation and records who took it — it posts **nothing** to a status page. Customers see something only when you publish the incident and post a public update. An incident can run its whole internal lifecycle while staying `internal`.

## How an incident opens

A background writer scans every enabled monitor (not only status-page components). When a monitor sustains a bad state — `down`, `error`, or `degraded` — it opens one incident; a sustained recovery to `up` resolves it automatically (with no human resolver recorded). One open incident per monitor at a time; duplicate failures fold into it.

Visibility is derived at open time: if the monitor is a component of an enabled status page the incident opens `public`, otherwise `internal`. A monitor on no page still gets a fully tracked internal incident.

You can also declare an incident by hand from the console (`/incidents/declare`) — for a problem a monitor can't see, like a customer report or a partner outage. A manual incident may stand alone or link to a monitor, and opens `internal`.

Each incident carries a **severity** (`minor` / `major` / `critical`) and an **urgency** (`high` pages on-call, `low` notifies only). A declared incident takes the severity you choose; an auto-opened one currently defaults to `major` until an operator changes it.

## The console

`/incidents` is the operator console — a management surface distinct from the dashboard's at-a-glance banner. It lists incidents with severity, state, monitor, assignee, and age, filterable by state. `/incidents/{id}` is the detail view: header, the action bar, the trigger sample, and the activity log.

The action bar drives the lifecycle:

| Action | Effect |
|---|---|
| **Acknowledge** | `state = acknowledged`, records the first acker, stops escalation. Re-acking keeps the original acker and time. |
| **Resolve** | `state = resolved`, records the resolver. (A sustained recovery auto-resolves with no resolver.) |
| **Reopen** | A resolved incident returns to `triggered` and re-arms escalation. |
| **Assign / unassign** | Set or clear the owning responder. |
| **Add note** | Free-text entry on the internal timeline. |

Acknowledge and resolve prompt for an optional note so you can capture the *why* at the moment you act.

### The activity log

Every lifecycle action writes an append-only event to the incident's internal timeline. Each entry answers **who, when, and what**: the acting member's email (system-driven transitions show `system`; an action taken through the MCP server is badged `via MCP`), an exact timestamp, and any note. This is the audit trail — the foundation for tracking response is a healthy habit of leaving notes, and the log makes that habit visible.

## Paging and escalation

When an incident opens, the escalation engine pages the responsible channels. Paging reuses the existing Slack / Telegram / WhatsApp / Webhook transports (see [Configuration](configuration.md)); email and SMS are not wired yet.

An **escalation policy** is an ordered ladder of levels. Each level waits a delay, then pages its targets; if no one acknowledges, the engine advances to the next level, and can repeat the ladder a configured number of times before giving up. Acknowledging the incident halts the walk.

A policy's targets can be:

- **a channel** — pages that notification channel directly;
- **a user** — pages the channels that member has chosen to be reached on (see on-call below);
- **a schedule** — resolves who is on call right now and pages them.

Policies are owner-managed at `/settings/escalation`: build the ladder, set per-level targets, and pick an org-default policy. Bind a specific policy to a monitor from the monitor's edit form. Resolution at page time is: the monitor's own policy, else the org default, else **simple mode** — the monitor's bound notification channels are paged directly, with no laddered re-paging.

> **One notification source.** Every down/up notification flows through the incident engine — there is no separate per-monitor alert dispatch, so a monitor can never double-page. The `escalation.enabled` switch gates only the policy machinery (ladder walk, policy UI); with it off, monitors still page their bound channels in simple mode.

While an incident stays **unacknowledged**, the engine re-sends a reminder on the monitor's `renotify_interval_secs` cadence (default hourly, `0` disables); acknowledging or resolving stops both the reminders and any escalation walk. Failed deliveries retry on exponential backoff and are dead-lettered after the attempt cap. Every attempt is auditable: the incident detail page has a **Delivery** section, and `GET /api/v1/incidents/{id}/notifications` returns the same log.

## On-call schedules

On-call schedules (owner-managed at `/settings/on-call`) decide *which human* a `user` or `schedule` target pages.

A schedule has a timezone and one or more **layers**. Higher layers win when stacked. Within a layer, participants rotate in listed order on a cadence:

| Rotation | Handoff |
|---|---|
| `daily` / `weekly` | Hands off at the same wall-clock time each period, in the schedule's timezone — stable across daylight-saving changes. |
| `custom` | A fixed number of seconds. |

**Overrides** cover a specific window with a chosen person (vacations, swaps) and beat the rotation while active. The editor's calendar builds one by clicking a start day, then an end day, then choosing who covers. A "who's on call now" widget resolves the current responder, and `GET /api/v1/on-call/who` answers it programmatically.

Resolution at page time, for a given instant: an override covering that instant wins; otherwise the highest layer that has participants, advanced by its rotation. The result is a set of users.

### Contact channels

A resolved user is paged through the org channels they have opted into — each member picks, on the on-call page, which notification channels reach them. A `user`/`schedule` target therefore resolves to people, then to their chosen channels; the paging log records the targeted user alongside the channel. If a member has chosen no channels, they resolve but cannot be paged.

## Publishing to a status page

Internal incidents never reach customers. Publishing is the explicit gate.

Every public read — the status page, its JSON API, the RSS feed, and the history markers — filters on `visibility = 'public'`, so an internal incident on a public-component monitor never leaks. Monitors that sit on an enabled status page open `public` automatically; everything else (manual incidents, monitors not on a page) stays internal until you publish.

From the incident detail page, **publish** flips visibility to `public` (optionally seeding a public title) and **unpublish** hides it again. A published incident appears on any status page whose components include its monitor. Narrate it for customers with public updates (the `investigating` → `monitoring` → `resolved` timeline); posting an update is separate from the internal state, exactly as the two-axis model intends.

## Postmortems

A resolved incident can carry one postmortem — a retrospective with a summary, root cause, impact, and a list of action items (each with optional owner and a done flag). Write it from the incident detail page (**write / edit postmortem**).

Publishing a postmortem surfaces it on the public incident page: customers see the summary, root cause, impact, and the action-item text and done state. Internal detail — the action-item *owner* — is never exposed publicly. A draft stays private until you publish, and publish/unpublish are recorded on the incident's activity timeline with the acting member, so the retrospective's own history is auditable.

## Metrics and reporting

`/incidents/reports` is a metrics dashboard over a trailing window (7 / 30 / 90 days):

- **MTTA** — mean time to acknowledge (`acknowledged_at − started_at`).
- **MTTR** — mean time to resolve (`ended_at − started_at`).
- Total incidents, counts by severity and by state, auto-resolved vs human-resolved, and the noisiest monitors.

The same numbers are available to automation through the MCP `get_incident_metrics` tool.

## MCP tools

An LLM connected through the [MCP server](mcp.md) can triage and operate incidents within its granted scopes: read the incident list and detail, read metrics, and — with write scope — acknowledge, resolve, and post public updates. Customer-supplied incident text is always returned as labelled data, never as instructions. See [MCP server](mcp.md) for the full tool table and scopes.

## Auth and scopes

| Surface | Requirement |
|---|---|
| Incident lifecycle (ack / assign / resolve / note / publish / declare) | `incidents:write` — any member; responders are not owners |
| Reading incidents and metrics | `incidents:read` |
| Escalation policies + on-call schedules (config) | `oncall:write` (owner-only); `oncall:read` to view |

There is no incident-delete: incidents are resolved, never deleted, to keep the audit trail intact. Owner and member are the only roles — any member can be assigned, put on a schedule, paged, and can operate an incident; owners manage the escalation/on-call configuration.

## Configuration

The `[escalation]` block (env prefix `UPTIMEPAGE_ESCALATION__*`) controls the engine:

| Key | Default | Purpose |
|---|---|---|
| `enabled` | `false` | Enable escalation policies (ladder walk + policy/on-call UI). Off, incidents still page the monitor's bound channels directly (simple mode). |
| `tick_interval_secs` | `15` | How often the engine sweeps for due escalations and failed-page retries. |
| `max_pages_per_tick` | `500` | Backpressure cap on pages re-sent per sweep. |
| `max_attempts` | `5` | Give up paging a channel after this many failed attempts. |

Per-org limits (`max_escalation_policies`, `max_on_call_schedules`, `on_call_enabled`) are plan quotas; see [Quotas & rate limits](quotas.md).
