# Deployment

## Production deployment with Caddy + basic auth

For real-world operation, use the production stack under `deployment/` in the repo. It puts a Caddy reverse proxy in front of the Rust service with:

- Automatic TLS via Let's Encrypt (HTTP/2 and HTTP/3 on by default)
- Basic auth on the UI and API
- Postgres and ClickHouse on the internal docker network — **no published ports**
- ClickHouse memory-capped at ~2 GB (see `deployment/clickhouse-config.xml`)

Setup:

```bash
cd deployment
cp .env.example .env
$EDITOR .env            # set domain, ACME email, bcrypt hash, DB passwords, KEK
docker compose up -d
```

`deployment/README.md` is the authoritative source for setup, user management, password rotation, backups, and troubleshooting.

### Authentication boundary

v1 has **no built-in auth** in the Rust service. The Caddy reverse proxy is the authentication boundary for the UI and API. `/healthz` and `/readyz` are intentionally exposed without auth so uptime probes, load balancers, and orchestrators can hit them. `/metrics` on the public domain returns 404 — scrape it on the internal docker network instead.

The public status page (`/status`, `/status/*`, `/api/public/*`, `/static/*`, `/robots.txt`, `/favicon.ico`) is **also unauthenticated by design** — see [Public status surface](#public-status-surface) below.

If native auth (session cookies, API tokens) is added later, the Caddy basic-auth layer can stay in place during the transition.

### Public status surface

The Caddyfile carries an `@public` matcher that short-circuits `basic_auth` for the public status paths and adds a per-IP rate limit (60 req/min) via the [`caddy-ratelimit`](https://github.com/mholt/caddy-ratelimit) plugin. The stock `caddy:2-alpine` image doesn't include that plugin, so the production deployment uses a custom `custom-caddy:2` image built with `xcaddy`:

```bash
docker build -t custom-caddy:2 - <<'EOF'
FROM caddy:2-builder AS builder
RUN xcaddy build --with github.com/mholt/caddy-ratelimit

FROM caddy:2-alpine
COPY --from=builder /usr/bin/caddy /usr/bin/caddy
EOF
```

Then point the `caddy` service in `deployment/docker-compose.yml` at `custom-caddy:2`. Full procedure (including the opt-out path that drops the rate-limit block) is in [`deployment/README.md`](https://github.com/slima4/status-monitor/tree/main/deployment).

For the operator workflow (enabling components, narrating incidents, scheduling maintenance) see [Public status page](public-status.md).

## Docker

`docker compose up -d` brings up Postgres 17, ClickHouse 25.8, and the monitor on the same network. Compose env vars wire the monitor to the stack:

```yaml
STATUS_MONITOR_STORAGE__POSTGRES__URL: postgres://monitor:monitor@postgres:5432/monitor
STATUS_MONITOR_STORAGE__CLICKHOUSE__URL: http://clickhouse:8123
STATUS_MONITOR_STORAGE__CLICKHOUSE__USER: monitor
STATUS_MONITOR_STORAGE__CLICKHOUSE__PASSWORD: monitor
STATUS_MONITOR_OBSERVABILITY__LOG_FORMAT: json
```

The runtime image is `gcr.io/distroless/static-debian12:nonroot` for a minimal attack surface, no shell, and no glibc. Built from a static musl binary via `rust:1-alpine`. Final image is **16 MB** — both `status-monitor` and `loadtest` binaries fit in the same image.

## Bind addresses

Defaults are loopback (`127.0.0.1:8080` API, `127.0.0.1:9090` metrics). Override via env for non-loopback exposure:

```bash
STATUS_MONITOR_SERVER__API_BIND=0.0.0.0:8080 \
STATUS_MONITOR_SERVER__METRICS_BIND=0.0.0.0:9090 \
./status-monitor
```

There is no built-in auth on the API port. Front it with a proxy or keep it on a private network. The ready-made Caddy stack under [`deployment/`](#production-deployment-with-caddy--basic-auth) does this for you.

## Migrations

- Postgres: `migrations/postgres/*.sql`, applied at startup via `sqlx::migrate!` (tracked in `_sqlx_migrations`)
- ClickHouse: `migrations/clickhouse/*.sql`, applied idempotently via `CREATE … IF NOT EXISTS` at startup

No external migrator. The app owns its schema lifecycle symmetrically.

## Resource sizing

- `checker.max_concurrent_checks` caps simultaneous in-flight checks
- Per-check memory: small (a tokio task + an in-flight hyper request + bookkeeping)
- The practical ceiling is set by file descriptors and ephemeral ports, not RAM
- At 50k concurrent checks against external targets, RSS sits around 200-400 MB depending on response sizes

## Graceful shutdown

The binary listens for SIGINT and SIGTERM, cancels the scheduler and batcher via a shared `CancellationToken`, awaits both background tasks, and exits within 10 s. The batcher's cancel branch drains any pending results before returning. A warning is logged if the deadline is exceeded.
