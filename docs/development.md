# Development

Local setup for iterating on the service. For production deployment see
[deployment.md](deployment.md).

## Prerequisites

- Rust 1.95+ (edition 2024) via `rustup`
- Docker + Docker Compose (for Postgres + ClickHouse)
- Optional: [`just`](https://github.com/casey/just) (`brew install just`) — every
  workflow below has a one-word `just` recipe equivalent. Run `just` to list
  them.

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

## Operator UI locally (SaaS mode)

The `dev-app` container runs in **SaaS mode** (`tenancy.enabled=true`, mirrors
the deployment). Self-host mode (`tenancy.enabled=false`) discards the session
cookie before reading it, so the owner-gated operator APIs — status-page
settings, members, invitations — return `401` and can't be exercised. The
host workflow (`cargo run` against `config/default.toml`) is self-host unless
you export the same env.

Get an authenticated owner session without GitHub OAuth:

```bash
just up-app          # SaaS-mode stack; wait for "api listening"
just dev-login       # seeds user+org+owner+session, prints the cookie
```

Then, in the browser devtools Console at `http://localhost:8080`:

```js
document.cookie = "_sm_session=devsession-localtest-0000000000; path=/";
```

Reload — you're the owner of "Dev Org". The public page is at
`http://devorg.status.lvh.me:8080/status` (`*.lvh.me` resolves to
`127.0.0.1`, no `/etc/hosts` edit). `just dev-login` also prints a `curl`
snippet that passes the cookie directly, for API-only checks.

After editing a migration in place (pre-launch policy), the dev DB trips
sqlx's "migration N modified" checksum guard — `just db-reset` drops and
recreates it (ClickHouse and the warm build cache are kept). `down -v` wipes
the seeded session; re-run `just dev-login`.

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
    "interval": 60, "enabled": true, "tags": [],
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

## Faster builds

```bash
just setup        # once: sccache + cargo-nextest, and the linker
                  # (mold on Linux; macOS prints an lld opt-in snippet)
just check        # primes test-profile artifacts so `just test` skips
                  # the rebuild a `cargo check` -> `cargo test` profile
                  # switch would otherwise force
```

- **Toolchain**: `rust-toolchain.toml` pins 1.95 for *every* entrypoint
  (bare `cargo`, `just`, rust-analyzer, CI) — no more ad-hoc `cargo +1.95`.
- **Linker**: `.cargo/config.toml` selects `mold` for Linux targets, so
  `just`, bare `cargo`, and rust-analyzer share one build fingerprint (an
  env `RUSTFLAGS` that differed between them would double-build `target/`).
  A Linux build needs `mold` installed — `just setup`. macOS is opt-in
  (Apple clang needs lld's machine-specific absolute path; `just setup`
  prints the `~/.cargo/config.toml` snippet).
- **sccache**: compile cache for local dev (`just` sets `RUSTC_WRAPPER`
  only when present) and CI (`mozilla-actions/sccache-action`, with
  `Swatinem/rust-cache` reduced to `cache-targets: false` so they don't
  double-store). Not in the release `Dockerfile` — cargo-chef already
  layer-caches deps there and the sccache mount wouldn't survive CI.
- CI installs the linker via `rui314/setup-mold`; the dev-app container
  via `apk add mold` + a persistent sccache volume.

## Tests

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release
cargo bench
```

Postgres-backed tests (e.g. `bulk_create_with_ragged_tags`) are `#[ignore]`'d
by default and no-op when `DATABASE_URL` is unset. Bring up the stack and opt
in. Validate schema/migration changes against a throwaway DB, not the stale
`monitor` one (the harness auto-applies migrations on first connect):

```bash
docker compose -f compose.dev.yml up -d
docker compose -f compose.dev.yml exec -T postgres createdb -U monitor ci_verify

# Whole ignored suite (slow — builds every test binary):
DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/ci_verify \
  cargo test -- --ignored

# One suite (fast — scope to a binary; bare `nextest run` rebuilds +
# enumerates all ~48 test binaries and looks frozen for minutes):
DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/ci_verify \
  cargo test --test status_page_settings_test -- --ignored --nocapture
```

## Database access

```bash
docker compose exec postgres psql -U monitor -d monitor
docker compose exec clickhouse clickhouse-client -u monitor --password monitor -d monitor
```

Same commands work against `compose.dev.yml`; the service names are identical.

## Tailwind / web UI

`build.rs` runs `./bin/tailwindcss --minify` before each `cargo build`. First
build fetches the standalone CLI (~30 MB) via `scripts/fetch-tailwind.sh`;
subsequent builds reuse it. Add a new utility class anywhere under
`templates/` and the next
build picks it up via the `@source` directive in `static/css/input.css`.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `503 STATUS_DATA_UNAVAILABLE` | Aggregator's first compute failed. Check `status_monitor::public_status::cache` ERROR log for the actual SQL/CH error. |
| `docker compose up --build` takes 5 min on every change | You're on the pre-cargo-chef Dockerfile. Pull latest. |
| Native `cargo run` fails with `Connection refused` | `compose.dev.yml` isn't up, or you forgot to release port 8080 from a running container. |
