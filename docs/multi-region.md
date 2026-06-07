# Multi-region probes

Run checks from more than one location and keep every result attributed to the region that produced it. A single **control plane** owns all state (Postgres, ClickHouse, the web UI, alerting, and a scheduler for its own region); additional boxes run as stateless **agents** that pull their region's monitor config and ship results back.

This is opt-in. A default deployment is a single region — the control plane checks everything itself and nothing below changes.

## Model

- **Control plane** — one process holding Postgres + ClickHouse + the web UI + alerting + a scheduler. Its own region is a normal region row identified by `scheduler.region` (default `"default"`); rename it to a real location, it is not a sentinel.
- **Agent** — a process started with `[agent] enabled = true`. It runs no database, web UI, or alerting. It pulls its region's decrypted monitor config from the control plane over authenticated HTTPS, runs the checks locally, and POSTs results back to the central ingest API. Agents never touch ClickHouse or fire alerts.
- **Region is the partition key.** One agent per region needs no coordination — there is no leader election. (Running more than one agent in the same region, or more than one control plane, is out of scope for this version.)

New targets are assigned to `scheduler.default_region` (empty falls back to `scheduler.region`). At boot the control plane reconciles the configured region rows and backfills any unassigned target to the default region, so enabling regions never leaves a target unchecked.

## Running an agent

On the agent box, point at the control plane and name the region. The token carries the agent's capability — supply it by environment variable, never in a committed file:

```toml
[agent]
enabled = true
control_plane_url = "https://app.example.com"
region = "eu-west"
pull_interval_secs = 30
flush_interval_secs = 5
buffer_capacity = 10000
```

```bash
UPTIMEPAGE_AGENT__TOKEN=sm_agent_…   # the token minted by POST /operator/agents
```

The agent must reference a region and a token that already exist (see the operator surface below). Pull and ingest behaviour:

- **Pull** (`GET /api/agent/targets`) — `401`/`403` is terminal: the agent clears its cached config and pauses, so revoking or disabling the agent stops the probe. `5xx`/timeout is transient: it keeps serving the last-known config. Responses are content-hashed with an ETag, so a credential re-encrypt invalidates the cache even without a config change.
- **Ingest** (`POST /api/agent/results`) — region and agent id are taken from the token, never trusted from the body. Rows that are clock-skewed or belong to a region the agent isn't assigned are dropped per-row (the rest of the batch still lands) and counted, rather than rejecting the whole batch. Cross-process de-duplication is authoritative in ClickHouse; a re-sent identical batch is idempotent.

## Operator surface

Regions and agents are managed instance-wide (across all tenants) under `/operator/*`, gated by a static bearer secret. Set it by environment variable; an empty value disables the surface entirely (it `404`s, so it is invisible when off):

```bash
UPTIMEPAGE_OPERATOR__ADMIN_TOKEN=…
```

```
Authorization: Bearer <that-secret>
```

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/operator/regions` | list regions |
| `POST` | `/operator/regions` | create a region (`id` is a `[a-z0-9-]` slug, `name`, optional `location`) |
| `PATCH` | `/operator/regions/{id}` | rename / relocate a region |
| `DELETE` | `/operator/regions/{id}` | delete a region — `409` while it still holds agents or assigned targets |
| `GET` | `/operator/agents` | list agents |
| `POST` | `/operator/agents` | mint an agent — the response carries its `sm_agent_…` token **once** |
| `PATCH` | `/operator/agents/{id}` | rename / enable / disable an agent |
| `DELETE` | `/operator/agents/{id}` | delete an agent |

The agent token is shown only at creation; store it when it is minted. Disabling an agent is immediately enforced on its next pull. There is no token-rotation endpoint yet — rotate by deleting and re-creating the agent.

A typical bring-up: create the region, mint an agent in it, copy the token to the agent box's `UPTIMEPAGE_AGENT__TOKEN`, start the agent.

## Viewing per-region data

Once results carry a region, the operator surfaces let you slice by it:

- **Dashboard** — a `region:` filter in the subhead (shown only when the org spans more than one region) scopes every fleet metric to one region. `?region=` is reflected in the URL.
- **Monitor detail** — a region selector scopes the KPI cards, latency and breakdown charts, and recent results. In the all-regions view the latency chart overlays one p95 line per region, and a **by region** table summarises uptime, p50, p95, and last status per region. Pick a region to drill into a single line.
- **REST API** — `/api/v1/targets/{id}/results`, `/latency`, and `/uptime` accept an optional `region=` query parameter; `/api/v1/targets/{id}/latency/by-region` returns one series per region. See [REST API](api.md#latency-series).

What deliberately **blends** across regions: the public status page's component status (the public "is it up" answer is region-agnostic by design), the monitors list, and incident timelines. Those aggregate every region so a viewer sees one verdict.

See [Configuration](configuration.md) for the `[scheduler]`, `[agent]`, and `[operator]` keys, and [Architecture](architecture.md) for where the pieces sit.
