# Development

Local setup for iterating on the service. For production deployment see
[deployment.md](deployment.md).

## Prerequisites

- Rust 1.95+ (edition 2024) via `rustup`
- Docker + Docker Compose (for Postgres + ClickHouse)

## Two workflows

| | First build | Incremental | Notes |
|---|---|---|---|
| Host workflow | ~2 min | **~3 s** | `cargo run` natively; only deps in Docker. Best for iteration. |
| Docker dev (cargo-watch) | ~3 min | ~3 s | Source bind-mounted, rebuilds happen inside the container with a cached `target/`. Live reload. |
| Docker prod-shape | ~5 min | ~30 s | Rebuilds image. Matches the prod build. Use for CI-shaped smoke tests. |

### Host workflow (recommended for day-to-day)

Bring up just Postgres + ClickHouse:

```bash
docker compose -f compose.dev.yml up -d
```

Run the binary natively:

```bash
cargo run --bin status-monitor
```

`config/default.toml` already points at `localhost:5432` and `localhost:8123`,
so no env overrides are needed. Edit code → Ctrl-C → `cargo run` again.

Tear down (keeps DB volumes):

```bash
docker compose -f compose.dev.yml down
```

Wipe data too:

```bash
docker compose -f compose.dev.yml down -v
```

### Docker dev workflow (live reload inside a container)

Runs the binary inside a container that bind-mounts the repo and re-runs
`cargo run` via [`cargo-watch`](https://crates.io/crates/cargo-watch) on every
source change. The compiled `target/` and the linux Tailwind CLI live in named
volumes, so they persist across restarts and don't clash with the host build.

```bash
docker compose -f compose.dev.yml --profile dev-app up -d --build
docker compose -f compose.dev.yml logs -f status-monitor
```

First run takes ~3 min (toolchain + cargo-watch install + cold build + Tailwind
fetch). After that, edits to `src/`, `templates/`, or `static/css/input.css`
trigger an incremental rebuild + restart inside the container, typically
under 5 s.

Don't combine this with `cargo run` on the host — both bind 8080.

Stop just the app (keep pg + ch up):

```bash
docker compose -f compose.dev.yml stop status-monitor
```

### Docker prod-shape workflow (full stack via Dockerfile)

```bash
docker compose up -d --build status-monitor
```

The `Dockerfile` uses [`cargo-chef`](https://github.com/LukeMathWalker/cargo-chef)
to split dependency compile from app compile. The first build is slow; later
src-only edits skip the dep cook layer and finish in ~30 s.

If you have the host workflow running and want to switch to docker, stop the
native binary first to free port 8080 (or stop the docker service first to free
the host port).

## Verify it's up

```bash
curl http://localhost:8080/healthz   # liveness
curl http://localhost:8080/readyz    # readiness (DBs reachable)
```

Browse:

- `http://localhost:8080/` — operator dashboard
- `http://localhost:8080/status` — public status page
- `http://localhost:8080/docs` — Swagger UI

## Seed a target

```bash
curl -sS -X POST http://localhost:8080/api/v1/targets \
  -H 'content-type: application/json' \
  -d '{
    "name": "example",
    "check": {"type":"http","url":"https://example.com/","method":"GET",
              "timeout":5000,"follow_redirects":false,"max_redirects":0,
              "expected_status":{"kind":"exact","value":200},
              "headers":{},"verify_tls":true},
    "interval": 30, "enabled": true, "tags": [],
    "public_status": true
  }'
```

`public_status: true` makes the target appear on `/status` and addressable via
`/api/public/v1/badge.svg?component=<id>`.

## Logging

`docker-compose.yml` sets the default level to:

```
status_monitor=debug,sqlx=warn,hyper=warn,tower_http=info,info
```

For the host workflow, pass it directly:

```bash
RUST_LOG="status_monitor=debug,sqlx=warn" cargo run --bin status-monitor
```

`RUST_LOG` always wins over the config file. Anyhow errors are printed with
`{:#}` from the public-status cache, so the full context chain shows up
without re-running with backtraces.

Stream container logs:

```bash
docker compose logs -f status-monitor
```

## Tests

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
cargo bench
```

Postgres-backed tests (e.g. `bulk_create_with_ragged_tags`) are `#[ignore]`'d
by default. Bring up the compose stack and opt in:

```bash
docker compose -f compose.dev.yml up -d
DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/monitor \
  cargo test -- --ignored
```

## Database access

```bash
docker compose exec postgres psql -U monitor -d monitor
docker compose exec clickhouse clickhouse-client -u monitor --password monitor -d monitor
```

Same commands work against `compose.dev.yml`; the service names are identical.

## Tailwind / web UI

`build.rs` runs `./bin/tailwindcss --minify` before each `cargo build`. First
build fetches the standalone CLI (~30 MB) via
[`scripts/fetch-tailwind.sh`](../scripts/fetch-tailwind.sh); subsequent builds
reuse it. Add a new utility class anywhere under `templates/` and the next
build picks it up via the `@source` directive in `static/css/input.css`.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `503 STATUS_DATA_UNAVAILABLE` | Aggregator's first compute failed. Check `status_monitor::public_status::cache` ERROR log for the actual SQL/CH error. |
| `docker compose up --build` takes 5 min on every change | You're on the pre-cargo-chef Dockerfile. Pull latest. |
| Native `cargo run` fails with `Connection refused` | `compose.dev.yml` isn't up, or you forgot to release port 8080 from a running container. |
