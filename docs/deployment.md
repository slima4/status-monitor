# Deployment

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

There is no built-in auth on the API port. Front it with a proxy or keep it on a private network.

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
