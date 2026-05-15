# Status Monitor — Production Deployment

This directory contains the production deployment for status-monitor:
**Caddy reverse proxy** (TLS + basic auth) in front of the Rust service,
PostgreSQL, and ClickHouse.

## What this gives you

| Concern | How it's handled |
|---|---|
| TLS certificates | Automatic via Let's Encrypt — no manual renewal |
| HTTP/2 + HTTP/3 | Enabled by default in Caddy |
| Authentication | Basic auth at the proxy layer on `app.{domain}` (UI + operator API) |
| Public status surface | Self-host: `/status` on `app.{domain}`. SaaS: each org at `{slug}.status.{domain}` (wildcard) |
| TLS for status pages | Wildcard cert for `*.status.{domain}` via Let's Encrypt + Hetzner DNS-01 |
| Public rate limit | Per-IP 60 req/min on the public surface (custom Caddy image, built automatically) |
| Public health probes | `/healthz` and `/readyz` exposed without auth |
| Metrics scraping | Internal-only — `/metrics` returns 404 publicly |
| Security headers | HSTS, X-Frame-Options, Referrer-Policy, etc. |
| Access logging | JSON format, rotated automatically |
| Database exposure | Postgres + ClickHouse have no public ports |
| Credential storage | AES-256-GCM at rest (KEK in env) |
| ClickHouse memory | Capped at ~2 GB by default (see `clickhouse-config.xml`); adjust upward for larger hosts |

## Prerequisites

- A Linux host (any cloud, any VPS, your own metal — Hetzner CX22 at €5/mo is fine)
- Public IP with **ports 80 and 443 open**
- Docker 24+ and `docker compose` v2
- ~4 GB RAM and 20 GB disk
- DNS hosted on **Hetzner DNS** (the wildcard cert uses its DNS-01 API):
  - `app.{domain}` → A/AAAA to this host
  - `*.status.{domain}` → A/AAAA to this host (SaaS mode; the wildcard
    sends every `{slug}.status.{domain}` here and the app maps slug → org)
  - A Hetzner DNS API token with zone-edit scope, from
    <https://dns.hetzner.com/settings/api-token>, set as
    `HETZNER_DNS_API_TOKEN` in `.env`

  Self-host (single org) needs only the `app.{domain}` record and no DNS
  token — the status page is served at `https://app.{domain}/status`.

## First-time setup

### Custom Caddy image (automatic)

The stock `caddy:2-alpine` image lacks two plugins this deployment needs:

- [`caddy-dns/hetzner`](https://github.com/caddy-dns/hetzner) — solves the
  ACME DNS-01 challenge for the `*.status.{domain}` wildcard certificate
  (HTTP-01 cannot validate a wildcard).
- [`caddy-ratelimit`](https://github.com/mholt/caddy-ratelimit) — per-IP
  throttle on the public status surface.

`deployment/Dockerfile.caddy` bakes both in. `docker compose up -d` builds
it automatically and tags it `status-monitor-caddy:2` — there is no manual
one-time step. To rebuild after a Caddy or plugin bump:

```bash
docker compose build caddy && docker compose up -d caddy
```

### 1. Clone and enter the deployment directory

```bash
git clone <your-repo>
cd <repo>/deployment
```

### 2. Configure environment

```bash
cp .env.example .env
$EDITOR .env
```

Fill in every value. The file has inline instructions for each variable.

### 3. Generate your admin password hash

```bash
docker run --rm caddy:2-alpine caddy hash-password
# Enter password, get bcrypt hash
```

Copy the output (starts with `$2a$14$...`) into `STATUS_MONITOR_ADMIN_HASH`
in `.env`. **Wrap it in single quotes** to prevent docker-compose from
treating `$` as variable interpolation:

```
STATUS_MONITOR_ADMIN_HASH='$2a$14$abc...xyz'
```

### 4. Generate database passwords and KEK

```bash
# Run all three at once
{
  echo "POSTGRES_PASSWORD=$(openssl rand -base64 24)"
  echo "CLICKHOUSE_PASSWORD=$(openssl rand -base64 24)"
  echo "STATUS_MONITOR_CREDENTIALS_KEK_BASE64=$(openssl rand -base64 32)"
}
```

Copy the output into `.env`.

### 5. Validate the config

```bash
docker compose config
```

This expands all env vars and prints the effective config. If any required
variable is unset, you'll see a warning.

### 6. Start the stack

```bash
docker compose up -d
docker compose logs -f caddy
```

Watch the Caddy logs. On first start it will:
1. Issue the `app.{domain}` cert via HTTP-01 (~30-60 seconds)
2. Issue the `*.status.{domain}` wildcard via the Hetzner DNS-01 challenge:
   Caddy writes a `_acme-challenge.status.{domain}` TXT record through the
   Hetzner API, Let's Encrypt validates it, the wildcard cert is issued —
   **allow 60-90 seconds** for this one; renewals are silent.
3. Bind to ports 80 and 443 and start proxying

When you see `serving initial configuration`, visit
`https://app.{domain}` — your browser will prompt for credentials.

#### Verify the wildcard cert (manual test)

```bash
# Operator cert
echo | openssl s_client -connect app.example.com:443 2>/dev/null \
    | openssl x509 -noout -subject

# Wildcard cert — any slug, even one that doesn't exist as an org, must
# present a *.status.example.com cert (the app returns 404 for unknown
# slugs, but TLS is served by the wildcard regardless).
echo | openssl s_client -servername anything.status.example.com \
    -connect anything.status.example.com:443 2>/dev/null \
    | openssl x509 -noout -subject
# Expect: subject=CN=*.status.example.com
```

If the wildcard line fails, grep the logs for the DNS-01 exchange:

```bash
docker compose logs caddy | grep -i "acme\|dns\|hetzner\|challenge"
```

Common causes: token missing/insufficient scope, the domain's
authoritative DNS is not Hetzner, or `STATUS_MONITOR_DOMAIN` still set to
a sub-host (it must be the base domain, e.g. `example.com`). While
debugging, switch to the staging CA (see "Testing the TLS flow" below) so
you don't burn production rate limits.

## Operations

### Adding a user

1. Generate a hash:
   ```bash
   docker run --rm caddy:2-alpine caddy hash-password
   ```

2. Add to `.env`:
   ```
   STATUS_MONITOR_OPERATOR_HASH='$2a$14$...'
   ```

3. Uncomment the corresponding line in `Caddyfile`:
   ```caddy
   basic_auth {
       admin {$STATUS_MONITOR_ADMIN_HASH}
       operator {$STATUS_MONITOR_OPERATOR_HASH}   # <-- uncomment
   }
   ```

4. Reload Caddy (no downtime, no restart):
   ```bash
   docker compose exec caddy caddy reload --config /etc/caddy/Caddyfile
   ```

### Removing a user

Delete the line from `Caddyfile`, then reload Caddy as above. The variable
in `.env` can stay; nothing references it once removed from Caddyfile.

### Rotating a password

Generate a new hash, update the value in `.env`, then:

```bash
docker compose up -d caddy   # picks up new env, restarts only caddy
```

Active sessions are not invalidated — basic auth is stateless, so the
next request from clients using the old password fails with 401.

### Calling the API from scripts / CI

```bash
curl -u admin:password https://app.example.com/api/v1/targets
```

Or with an explicit header:

```bash
curl -H "Authorization: Basic $(echo -n admin:password | base64)" \
     https://app.example.com/api/v1/targets
```

If/when the service grows native auth (API tokens, session cookies), the
basic auth layer at the Caddy edge can stay in place during the transition.

### Scraping metrics from Prometheus

Metrics are at `http://status-monitor:9090/metrics` on the **internal docker
network**. The simplest setup: add a Prometheus service to the same
docker-compose stack. The compose file has a commented-out example you can
uncomment.

If you must scrape from outside the host, use a separate Caddy site with
its own basic_auth and an aggressive IP allowlist — never expose `/metrics`
on the public domain.

## Testing the TLS flow without rate-limit pain

Let's Encrypt production has rate limits (50 certs/week per registered
domain). While iterating, switch to staging:

In `Caddyfile`, uncomment:

```caddy
acme_ca https://acme-staging-v02.api.letsencrypt.org/directory
```

Restart Caddy. You'll get staging certs that browsers don't trust, but the
full cert flow runs. Switch back to production when ready.

If you've already issued a staging cert and want to switch to production,
**delete the caddy_data volume** to force re-issuance:

```bash
docker compose down
docker volume rm status-monitor_caddy_data
docker compose up -d
```

## Backups

Three volumes need backing up:

| Volume | Contains | Frequency |
|---|---|---|
| `caddy_data` | TLS certs and account keys (~10 MB) | Weekly |
| `postgres_data` | Target configuration | Daily |
| `clickhouse_data` | Check results history | Daily (or accept the loss) |

Postgres backup (consistent, no downtime):

```bash
docker compose exec -T postgres pg_dump -U monitor monitor | \
    gzip > "backups/postgres-$(date +%Y%m%d).sql.gz"
```

ClickHouse backup is bigger and more involved — use ClickHouse's
`BACKUP TABLE check_results TO Disk(...)` from inside the container.
See ClickHouse docs.

Caddy data is small — just copy the volume:

```bash
docker run --rm -v status-monitor_caddy_data:/source:ro \
    -v "$(pwd)/backups":/backup alpine \
    tar czf /backup/caddy-$(date +%Y%m%d).tar.gz -C /source .
```

## Upgrading

```bash
docker compose pull
docker compose up -d
```

For Caddy config changes (Caddyfile only, no env changes), use a hot reload:

```bash
docker compose exec caddy caddy reload --config /etc/caddy/Caddyfile
```

For status-monitor upgrades, do a normal `up -d`. The new container is
distroless so Docker has no container-level healthcheck to gate on;
instead, Caddy's active `health_uri /healthz` probe (every 30s) pulls
the upstream out of rotation while it's down and back in once it
recovers. There's a brief window of 502 responses during the swap —
typically one health interval. If you need zero-downtime upgrades, run
two status-monitor replicas behind Caddy's `reverse_proxy` load
balancer (Caddy supports this with multiple upstreams).

## Troubleshooting

**Certificate fails to provision**
- DNS not propagated? `dig +short app.example.com` and
  `dig +short anything.status.example.com` (the wildcard must resolve)
- Wildcard cert stuck? Authoritative DNS must be Hetzner and the token
  needs zone-edit scope — `docker compose logs caddy | grep -i hetzner`
- Ports 80/443 blocked? Test from another host: `curl -v http://app.example.com`
- Hit Let's Encrypt rate limit? Switch to staging (above) while you fix things
- Cloud firewall rules? Check security groups / firewall settings

**"Invalid credentials" but the password is correct**
- Bcrypt hash in `.env` not wrapped in single quotes? `$` got interpolated. Wrap it.
- Hash was generated for a different cost? Caddy accepts any valid bcrypt hash. Regenerate.

**Caddy can't reach status-monitor**
```bash
docker compose ps                                  # both running?
docker compose exec caddy wget -qO- http://status-monitor:8080/healthz
```

**status-monitor logs show "ClickHouse unreachable"**

The status-monitor image is distroless (no shell, no wget) so probe from
the caddy container, which shares the same docker network:

```bash
docker compose exec caddy wget -qO- http://clickhouse:8123/ping
```
Often a password mismatch. Re-check `CLICKHOUSE_PASSWORD` in `.env` matches
what the container actually uses (you may need to wipe the volume after
changing — Postgres and ClickHouse only honor `*_PASSWORD` env vars at
first init).

**High memory use on small VMs**
The stack ships with `clickhouse-config.xml` capping ClickHouse at ~2 GB by
default, which is enough for ~50M rows/day. If you run on a larger host
(8 GB+ available for ClickHouse alone), edit `deployment/clickhouse-config.xml`
to raise `max_server_memory_usage` and `max_memory_usage`, or delete the
file entirely to use ClickHouse defaults.

## Lighthouse audit (public status page)

The public status page targets a Lighthouse accessibility score ≥ 95.
Run the audit against a live deployment — Lighthouse needs a real
HTTP origin, not an in-process router. There is no Rust harness for this;
use the `lighthouse` Node CLI on any workstation. Audit the URL your mode
serves: self-host `https://app.example.com/status`, SaaS
`https://{slug}.status.example.com`.

```bash
# One-shot install + run; outputs JSON + HTML reports
npx -y lighthouse@12 https://app.example.com/status \
    --only-categories=accessibility,performance,best-practices,seo \
    --output=json,html \
    --output-path=./lighthouse-status \
    --chrome-flags="--headless=new --no-sandbox" \
    --quiet
```

Capture the four category scores from `lighthouse-status.report.json`:

```bash
jq '.categories | to_entries[] | "\(.key): \(.value.score * 100)"' \
    lighthouse-status.report.json
```

Re-run after any template, CSS, or `static/js/public/*` change. The page
ships with HTMX + ~35 lines of timezone JS — under 30 KB gzipped — so
the performance category typically lands in the high 90s on a wired
connection. If accessibility drops below 95, the report's `audits`
section lists the failing rules with element selectors; the public
templates live in `templates/public/`.

## What's intentionally NOT here

This deployment is right-sized for **single-tenant, small-team operator use**:

- **No load balancer.** Caddy on one host handles 10k+ concurrent
  connections. If you need geographic redundancy, run independent
  status-monitor instances per region.
- **No HA database.** Single Postgres, single ClickHouse. If either dies,
  the service degrades to read-only or stops accepting writes. For HA,
  switch to managed Postgres (Neon, RDS) and managed ClickHouse (ClickHouse
  Cloud, Altinity) — but you lose the "single VM, $20/month" property.
- **No SSO.** Basic auth is the current auth boundary. Native session
  cookies or API tokens are not part of this stack.
- **Rate limiting only on the public surface.** The operator UI/API has no
  per-IP throttle — basic auth is the gate. Add `caddy-ratelimit` zones to
  the auth-gated `reverse_proxy` block if you need it.
- **No WAF / DDoS protection.** Front this with Cloudflare (free tier) if
  you need that — Caddy's basic_auth is not designed to absorb
  credential-stuffing attacks at scale.

These are deliberate omissions, each documented as a known gap.
The deployment as specified is good enough for a fleet of up to ~100k
monitored targets on a single host.

## File reference

| File | Purpose |
|---|---|
| `Caddyfile` | Caddy v2 reverse proxy config (operator + wildcard status) |
| `Dockerfile.caddy` | Custom Caddy image (Hetzner DNS-01 + rate-limit plugins) |
| `docker-compose.yml` | Service definitions for the full stack |
| `.env.example` | Template for `.env` (commit this; never commit `.env`) |
| `README.md` | This file |
