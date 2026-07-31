# MCP server

uptimepage exposes a [Model Context Protocol](https://modelcontextprotocol.io/docs/getting-started/intro) server so an LLM client — the [claude.ai](https://claude.ai) connector, Claude Desktop, an IDE, or MCP Inspector — can answer operational questions about **one organization** and take a few guarded actions, through typed, authorized, audited tools.

It is another authorized front door to the same stores the web app and [`/api/v1`](api.md) use, not a bypass: tenant isolation, scopes, rate limits, and audit all apply. Every tool takes the org from the credential — never from a tool argument — so a connection can only ever see and touch its own org.

- **Transport** — Streamable HTTP at `POST/GET /mcp`, served on its own host (`mcp.{DOMAIN}` in production).
- **Auth** — an org-bound scoped API token (`sm_live_…`), minted either by hand (Settings → API tokens) or by the one-click OAuth 2.1 connector flow.
- **Surface** — 10 read tools (always) + 6 write tools (each scope-gated, confirmed per action, and audited).

The server only mounts when enabled (see [Enabling](#enabling)); a deployment that leaves it off never exposes `/mcp`.

## Tools

All tools return typed `structuredContent`. Customer free text (monitor names, group names, tags, error messages, incident text) is returned as **labelled data**, never as instructions to the model — the server's instructions tell the client to treat it that way.

### Read tools

Side-effect-free (`readOnlyHint`). Require `targets:read`, `status_page:read`, or `incidents:read`.

| Tool | Scope | Returns |
|---|---|---|
| `get_org_health` | `targets:read` | Per-state monitor totals + the worst currently-failing monitors, each with its open `incident_id`. The one-shot "what is broken right now?" answer — start here. |
| `list_monitors` | `targets:read` | Monitors with optional `state` / `type` / `tag` filters, cursor-paginated; each item carries current state + last-checked time. |
| `get_monitor` | `targets:read` | One monitor's config, current state, last error, last HTTP status, and 24h / 30d uptime. |
| `get_monitor_history` | `targets:read` | One monitor's history over a `window` (`1h` / `24h` / `7d` / `30d`): uptime, latency series, failures with error text, incident windows. |
| `get_flow_runs` | `targets:read` | A browser flow monitor's recent runs over a `window`: every declared step with its outcome and duration, the step a failure stopped on, and the page the browser saw. Answers *why* a login check failed. |
| `get_flow_step_trend` | `targets:read` | Per step of a browser flow monitor, over a `window`: earliest and latest mean duration, the ratio between them, and how many runs passed or failed it. Answers *which step is getting slower* while the monitor still reports up. |
| `list_incidents` | `incidents:read` | Currently-open incidents on the org's status pages: incident id, affected monitor, severity, latest update phase. Cursor-paginated. |
| `get_incident` | `incidents:read` | One incident: affected monitor, severity, open/resolved times, error sample, and the full operator-update timeline. |
| `get_incident_metrics` | `incidents:read` | Incident metrics over a trailing window (default 30 days): MTTA/MTTR, total, counts by severity and state, auto- vs human-resolved, and the noisiest monitors. |
| `list_status_pages` | `status_page:read` | The org's status pages: slug, name, public URL, enabled. Cursor-paginated. |
| `get_status_page` | `status_page:read` | One status page with its components and each linked monitor's current state. |
| `get_org_usage` | `targets:read` | Resource usage against plan limits (monitors, status pages, members, components) + key policy values. |

A status-page monitor is down → `get_org_health` gives the `incident_id` → `get_incident` shows the timeline → `acknowledge_incident` takes ownership → `post_incident_update` tells your customers. Incidents (and the `incident_id` / ack workflow) exist only for monitors that are status-page components; a monitor not on any status page can be failing with `incident_id: null` — `since` still reports how long it's been down. `run_check_now` and `get_monitor` return `http_status` for HTTP monitors so you can tell "wrong status code" from "no response".

### Write tools

Not read-only. Each requires its scope **and** an interactive [confirmation](#confirmations) before it runs, and writes exactly one [audit](#audit) row for every outcome (success, declined, denied, error).

| Tool | Scope | Effect |
|---|---|---|
| `run_check_now` | `targets:execute` | Probe a monitor immediately and record the result. A `down` result may fire the org's normal alerts. |
| `pause_monitor` | `targets:write` | Stop a monitor's checks until resumed. Idempotent. |
| `resume_monitor` | `targets:write` | Restart a paused monitor's checks. Idempotent. |
| `acknowledge_incident` | `incidents:write` | Take ownership of an incident and halt escalation. Internal only: it posts nothing to the public status page. Idempotent. |
| `resolve_incident` | `incidents:write` | Mark the incident resolved. Internal only, same as acknowledge: the public page is untouched. Idempotent. |
| `post_incident_update` | `incidents:write` | Post the customer-facing update to the incident's status-page timeline: a `message` plus optional `phase` (`investigating` / `identified` / `monitoring` / `resolved` / `postmortem`, default `investigating`). |

Write scopes are **never** granted unless explicitly requested — the OAuth connector defaults to read-only (see [Scopes](#scopes)).

## Authentication

The `/mcp` endpoint is an OAuth 2.1 protected resource. It accepts an `Authorization: Bearer sm_live_…` token that must be:

- a live scoped [API token](authentication.md#api-token-auth),
- **bound to one org** (an unbound token is rejected — the connection has no org header to fall back on), held by a current member of that org,
- carrying the scope each tool requires (else `403 insufficient_scope`), and
- when OAuth is configured, stamped with this endpoint as its `audience` (RFC 8707) — a token minted for a different audience is refused.

A request with no/invalid token gets `401` with a `WWW-Authenticate: Bearer …` header pointing at the resource metadata, which kicks off discovery for OAuth clients.

### Two ways to get a token

**1. By hand (manual connector).** Mint an org-bound, read-only, expiring token in the UI (Settings → API tokens; a verified email is required) and paste it into the client. Grant the least scope you need — `targets:read` + `status_page:read` + `incidents:read` for the read tools. This is the simplest path for Claude Desktop / Inspector and needs only `UPTIMEPAGE_MCP_ENABLED`.

**2. One-click OAuth (claude.ai connector).** With `UPTIMEPAGE_MCP_OAUTH_ENABLED` on, the client discovers the authorization server, you log in with your existing session and approve a consent screen, and the server mints the same org-bound expiring token behind the scenes — no copy-paste. This is the only path that mints write scopes, and only when the consent screen's opt-in boxes are checked.

### Why OAuth at all?

The manual path works but pushes a long-lived bearer token through copy-paste and client config. OAuth replaces that with a browser consent: the user authenticates against the existing login, the connector receives a short-lived access token plus a rotating refresh token, and the **connection lifetime** (refresh-token lifetime) is the user's explicit choice on the consent screen (default 90 days, max 365 — there is deliberately no "never"). Reused refresh tokens revoke the whole family. The connector never sees the user's password and the access token is bound to this one resource.

### OAuth endpoints

Discovery + authorization-server endpoints live on the **app** host (where the session cookie lives); the protected resource is `/mcp` on its own host.

| Endpoint | Host | Purpose |
|---|---|---|
| `/.well-known/oauth-protected-resource` | resource (`mcp.`) | RFC 9728 resource metadata (resource id, authorization servers, scopes) |
| `/.well-known/oauth-authorization-server` | app | RFC 8414 AS metadata (PKCE S256 only, public clients, code + refresh grants) |
| `/oauth/register` | app | RFC 7591 Dynamic Client Registration |
| `/oauth/authorize` | app | Login + consent screen (PKCE S256, RFC 8707 `resource`) |
| `/oauth/token` | app | Issue / refresh the audience-bound token |

Redirect URIs are restricted to HTTPS hosts (web connectors) and loopback HTTP (local tooling); custom schemes, non-loopback cleartext, userinfo, and fragments are rejected at registration.

### Consent screen

`GET /oauth/authorize` renders the consent screen — the one page the user sees during the OAuth flow. It appears **after login**, once the client + redirect URI are validated, and only when `mcp.oauth_enabled` is on. Approving here is what mints the token; nothing is granted until the user clicks Approve.

It shows:

- **Who and what** — the client name and the single org it's connecting to. Access is always scoped to that one org.
- **Granted abilities** — one line per scope, in plain language (e.g. "Read your monitors and their current status", "Pause and resume your monitors"). Write abilities are flagged with a ⚠ marker, and a warning banner appears at the top stating the connection can make changes — each of which still asks for per-action confirmation.
- **Connection expires** — a picker (30 / 60 / 90 / 365 days, default 90) that sets the refresh-token (connection) lifetime. There is no "never".
- **Approve / Deny** — Deny aborts the flow; Approve mints the org-bound scoped token and returns the user to the client.

A read-only request shows "wants read-only access" with no warning banner; a request that includes any write scope switches to the "is requesting access" wording plus the banner and ⚠ markers.

## Scopes

The connector advertises six grantable scopes. A request with no `scope` (or only unknown scopes) grants the **read-only default**; write scopes are opt-in.

| Scope | Grants | In default set? |
|---|---|---|
| `targets:read` | all read tools over monitors | ✅ |
| `status_page:read` | status-page read tools | ✅ |
| `incidents:read` | `list_incidents`, `get_incident` | ✅ |
| `targets:write` | `pause_monitor`, `resume_monitor` | opt-in |
| `targets:execute` | `run_check_now` | opt-in |
| `incidents:write` | `acknowledge_incident` | opt-in |

A granted write scope is **necessary but not sufficient** — every write tool still asks the user to confirm the specific action at call time.

## Confirmations

Before any write tool acts, the server sends an MCP **elicitation** request describing the exact action (the monitor's name, the effect, and — for `acknowledge_incident` — the message and notify choice). The tool proceeds only on an explicit approval; a decline, a dismissal, or a client that can't elicit all fail closed with `not_confirmed`. There is no "remember my choice" — each action is confirmed on its own.

## Audit

Every write-tool invocation writes one row to `mcp_audit`, on **every** path — success, user-declined, scope-denied, bad input, not-found, or server error — recording: `actor_type = mcp`, the token id, the acting user + org, the tool name, the arguments, the outcome (`success` / `denied` / `error`), and a short detail code. The same event is emitted to tracing. Reads are not audit-logged (they're side-effect-free and already rate-limited).

## Enabling

Off by default. Config keys (TOML under `[mcp]`, or env with the `UPTIMEPAGE_` prefix and `__` nested separator):

| Key | Env | Default | Purpose |
|---|---|---|---|
| `mcp.enabled` | `UPTIMEPAGE_MCP__ENABLED` | `false` | Mount `/mcp` + the read/write tools. |
| `mcp.oauth_enabled` | `UPTIMEPAGE_MCP__OAUTH_ENABLED` | `false` | Add the OAuth 2.1 endpoints that back the one-click connector. |
| `mcp.resource_uri` | `UPTIMEPAGE_MCP__RESOURCE_URI` | _empty_ | Canonical absolute URI of `/mcp` — the OAuth resource id + RFC 8707 audience, e.g. `https://mcp.uptimepage.dev/mcp`. Empty disables audience binding (static-token mode). |
| `mcp.allowed_origins` | `UPTIMEPAGE_MCP__ALLOWED_ORIGINS` | _empty_ | RFC 6454 Origin allow-list (DNS-rebinding defense). Empty disables the check; a missing `Origin` header always passes (non-browser clients send none). |
| `mcp.access_token_ttl_secs` | `UPTIMEPAGE_MCP__ACCESS_TOKEN_TTL_SECS` | `3600` | Access-token lifetime (short; auto-renewed via the rotating refresh token). |

When OAuth is on, the app **refuses to boot** unless `mcp.resource_uri` and `auth.public_base_url` are real HTTPS origins — the issuer and audience must be well-formed. Migrations `016` (OAuth) + `017` (audit) must be applied.

### Production (GitHub-managed)

The deploy pipeline upserts the two switches from repo **variables** (Settings → Secrets and variables → Actions → Variables):

- `MCP_ENABLED=true`
- `MCP_OAUTH_ENABLED=true`

`deploy.yml` writes the corresponding `UPTIMEPAGE_MCP_*` keys into the server `.env` on each deploy. The resource URI defaults to `https://mcp.{UPTIMEPAGE_DOMAIN}/mcp`; `mcp.{DOMAIN}` rides the existing `*.{DOMAIN}` wildcard cert + Caddy route (no new DNS). See `deployment/.env.example` and [Deployment](deployment.md).

## Connecting a client

### claude.ai connector (OAuth)

Settings → Connectors → Add custom connector → URL `https://mcp.{DOMAIN}/mcp` → Connect. You'll be sent to the login + consent screen; approve, and the tools appear. This exercises the full OAuth path and is the recommended end-user flow.

### Claude Desktop / IDE (manual token via mcp-remote)

`mcp-remote` bridges a local stdio client to the remote Streamable HTTP endpoint. Add to your client config:

```jsonc
{
  "mcpServers": {
    "uptimepage": {
      "command": "npx",
      "args": [
        "-y", "mcp-remote",
        "https://mcp.uptimepage.dev/mcp",
        "--header", "Authorization: Bearer sm_live_YOUR_TOKEN"
      ]
    }
  }
}
```

For a local dev server over plain HTTP, add `--allow-http` to the args.

### MCP Inspector (testing)

```bash
npx @modelcontextprotocol/inspector
```

Set transport **Streamable HTTP**, URL `https://mcp.uptimepage.dev/mcp`, and an `Authorization: Bearer sm_live_…` header. Inspector lists every tool with its schema and lets you exercise the elicitation approve/deny flow.

## Examples

### Raw protocol (curl)

The transport is JSON-RPC over Streamable HTTP. `initialize` returns a session id the client echoes on later calls.

```bash
# initialize → 200 + Mcp-Session-Id response header
curl -sD- https://mcp.uptimepage.dev/mcp \
  -H 'Authorization: Bearer sm_live_YOUR_TOKEN' \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize",
       "params":{"protocolVersion":"2025-11-25",
                 "capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'

# list tools (reuse the session id from the initialize response)
curl -s https://mcp.uptimepage.dev/mcp \
  -H 'Authorization: Bearer sm_live_YOUR_TOKEN' \
  -H 'Mcp-Session-Id: THE_SESSION_ID' \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

# call a tool: open incidents on your status pages
curl -s https://mcp.uptimepage.dev/mcp \
  -H 'Authorization: Bearer sm_live_YOUR_TOKEN' \
  -H 'Mcp-Session-Id: THE_SESSION_ID' \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call",
       "params":{"name":"list_incidents","arguments":{}}}'

# read one incident's timeline (id from list_incidents or get_org_health)
curl -s https://mcp.uptimepage.dev/mcp \
  -H 'Authorization: Bearer sm_live_YOUR_TOKEN' \
  -H 'Mcp-Session-Id: THE_SESSION_ID' \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call",
       "params":{"name":"get_incident","arguments":{"id":"INCIDENT_ID"}}}'
```

Write tools (`acknowledge_incident`, `pause_monitor`, …) follow the same `tools/call` shape but the client must support [elicitation](#confirmations) — curl can't approve the confirmation, so they're driven from a real MCP client.

A missing/invalid token returns `401` with `WWW-Authenticate: Bearer …`; a wrong `Host` returns `403`; a missing `MCP-Protocol-Version` on a non-initialize call returns `400`; notifications get `202`.

### Asking an LLM

Once connected, drive it in natural language — the client picks the tool:

- "What's broken in my org right now?" → `get_org_health`
- "Show me every DNS monitor that's degraded." → `list_monitors(type=dns, state=degraded)`
- "How has the checkout API done over the last 7 days?" → `get_monitor_history(window=7d)`
- "Why did the login check fail last night?" → `get_flow_runs(window=24h)`
- "Is any step of the sign-in getting slower?" → `get_flow_step_trend(window=30d)`
- "What incidents are open, and what's been posted on them?" → `list_incidents` → `get_incident`
- "Acknowledge the payments incident — we're investigating." → `acknowledge_incident(phase=investigating)` (asks you to confirm)
- "Am I near any plan limits?" → `get_org_usage`
- "Run a check on the payments monitor now." → `run_check_now` (asks you to confirm; may alert)
- "Pause the staging monitor." → `pause_monitor` (asks you to confirm)

## Security model

- **Org isolation.** Org comes from the token, never an argument; the token must be org-bound and the holder a live member. The cross-tenant guarantees in [Multi-tenancy](multi-tenancy.md) apply unchanged.
- **Least privilege.** Read-only by default; write scopes are opt-in and each write is separately confirmed and audited.
- **Audience binding.** With OAuth on, tokens are pinned to this `/mcp` resource (RFC 8707), so a token leaked from elsewhere can't be replayed here.
- **DNS-rebinding defense.** The transport enforces a Host allow-list (the configured resource host) and an optional Origin allow-list.
- **Prompt-injection posture.** Customer-supplied text is returned as labelled data and the server instructions tell the client not to treat it as commands — but the ultimate guard is that the dangerous tools are scope-gated and human-confirmed.

## Related

- [Authentication](authentication.md) — scoped API tokens, org binding, expiry.
- [Multi-tenancy](multi-tenancy.md) — the isolation model every tool inherits.
- [Quotas & rate limits](quotas.md) — the per-plan limiter `/mcp` shares.
- [Configuration](configuration.md) — full config reference.
