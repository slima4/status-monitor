# Running a regional probe agent

A regional agent is the uptimepage binary run in agent mode on a box in another
region. It pulls its region's monitor config from the control plane, executes
the checks from that vantage point, and ships results back. It runs no web
server, database, or alerting — only outbound HTTPS to the control plane.

The control plane stays the single source of truth: config, storage, incidents,
and paging all live there. Adding a region adds execution capacity, never a
second control plane.

## Prerequisites

- The control plane is reachable over HTTPS at a stable origin (e.g.
  `https://app.uptimepage.dev`).
- The operator surface is enabled on the control plane: `UPTIMEPAGE_OPERATOR__ADMIN_TOKEN`
  is set. When it is empty the `/operator/*` endpoints return 404.
- Docker + the compose plugin on the agent box.

## 1. Create the region and mint the agent (on the control plane side)

Both calls are operator-only — authenticate with the admin token as a bearer.
Run them from anywhere that can reach the control plane.

```bash
CP=https://app.uptimepage.dev
OP=<UPTIMEPAGE_OPERATOR__ADMIN_TOKEN>

# Create the region (id is the slug agents bind to).
curl -fsS -X POST "$CP/operator/regions" \
  -H "Authorization: Bearer $OP" -H 'Content-Type: application/json' \
  -d '{"id":"eu-helsinki","name":"EU (Helsinki)","location":"Helsinki, FI"}'

# Mint an agent in that region. The token is returned ONCE — copy it now.
curl -fsS -X POST "$CP/operator/agents" \
  -H "Authorization: Bearer $OP" -H 'Content-Type: application/json' \
  -d '{"region":"eu-helsinki","name":"eu-helsinki-1"}'
# => {"id":"...","region":"eu-helsinki","name":"eu-helsinki-1",
#     "token":"sm_agent_...","token_prefix":"sm_agent_..."}
```

If the agent call returns `region not found`, the region id does not exist —
create it first.

## 2. Start the agent (on the agent box)

```bash
# Copy the compose file + env template to the box, then:
cp .env.agent.example .env
# Edit .env: set AGENT_TOKEN (the sm_agent_ token above), AGENT_REGION
# (eu-helsinki), AGENT_CONTROL_PLANE_URL, and pin UPTIMEPAGE_IMAGE to the
# same image the control plane runs.

docker compose -f docker-compose.agent.yml up -d
docker compose -f docker-compose.agent.yml logs -f agent
```

A healthy start logs `starting regional agent` then a config pull. Assign a
monitor to this region (monitor form → regions, or `PUT
/api/v1/targets/{id}/regions`) and within one pull interval the agent begins
checking it; results appear under that region on the dashboard and monitor
detail views.

## 3. Verify

- **Liveness:** the control plane bumps `agents.last_seen_at` on every pull and
  push. The `uptimepage_agent_up` and `uptimepage_agent_last_seen_age_seconds`
  Prometheus gauges expose staleness; wire a Grafana alert on them so a dead
  agent (a silent regional
  monitoring gap) pages. `agent_stale_after_secs` (operator config) sets the
  threshold the gauge flips at. The agent also serves its own metrics on `:9090`
  (internal network only) — scrape it for buffer depth and push outcomes if you
  run a metrics sidecar on the agent box.
- **Data:** pick a monitor assigned to the region and confirm its detail view
  shows a distinct per-region series.

## Rotating an agent token

There is no in-place rotation endpoint. Rotate by replacing the agent:

```bash
# 1. Mint a replacement in the same region (new token).
curl -fsS -X POST "$CP/operator/agents" \
  -H "Authorization: Bearer $OP" -H 'Content-Type: application/json' \
  -d '{"region":"eu-helsinki","name":"eu-helsinki-2"}'

# 2. Update .env on the box with the new AGENT_TOKEN, then recreate:
docker compose -f docker-compose.agent.yml up -d --force-recreate

# 3. Delete the old agent (its token stops working immediately).
curl -fsS -X DELETE "$CP/operator/agents/<OLD_AGENT_ID>" \
  -H "Authorization: Bearer $OP"
```

Disabling instead of deleting (`PATCH /operator/agents/{id}` with
`{"enabled":false}`) immediately rejects the agent's calls but keeps its result
history and excludes it from the region-silence alert.

## Retiring a region

Reassign or remove the region from every monitor first, then delete the agents,
then the region. `DELETE /operator/regions/{id}` reports the region as in-use
(FK) rather than cascading, so nothing is silently orphaned.
