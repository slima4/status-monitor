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
cargo run --bin uptimepage
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
docker compose -f compose.dev.yml logs -f uptimepage
```

First run takes ~3 min (toolchain + cargo-watch install + cold build + Tailwind
fetch). After that, edits to `src/`, `templates/`, or `static/css/input.css`
trigger an incremental rebuild + restart inside the container, typically
under 5 s.

Don't combine this with `cargo run` on the host — both bind 8080.

Stop just the app (keep pg + ch up):

```bash
docker compose -f compose.dev.yml stop uptimepage
```

### Docker prod-shape workflow (full stack via Dockerfile)

```bash
docker compose up -d --build uptimepage
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

## Operator UI locally

The `dev-app` container runs the same SaaS code path as production. The
host workflow (`cargo run` against `config/default.toml`) does too — the
binary is multi-tenant SaaS in every environment; a single-tenant deploy
is just a SaaS deploy with one signed-up user.

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
`http://devorg.lvh.me:8080/status` (`*.lvh.me` resolves to
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

## Seed UI fixtures

For end-to-end UI smoke (every public-page render path, varied check_spec
kinds, notification channels, alert bindings, maintenance binding, adversarial
title) use the bulk fixture script after `just dev-login`:

```bash
just seed-fixtures
```

What it seeds (under the `seed-fixtures` tag, idempotent):

- **14 monitors** — 8 public (covering all 5 component states: Operational /
  Degraded / Partial outage / Major outage / Maintenance — plus the
  disabled-target and ungrouped render paths) and 6 internal exercising every
  `check_spec` kind (http / tcp / dns / tls_cert / domain_expiry).
- **161 incidents** — 150 resolved across 87 days (cleared the 50-incident
  cap so the "Older incidents →" archive link renders), 10 active in mixed
  phases (investigating / identified / monitoring), 1 adversarial-title
  incident covering the day-popover JSON-escape path.
- **90-day ClickHouse history** — per-target divergent shape via
  `cityHash64(tid)` (each component has a distinct uptime% and outage
  pattern), an explicit 87-89d "ancient outage" cluster on the first three
  targets, and a 6-day NoData gap on fix-email.
- **9 notification channels** — one per `ChannelConfig` variant (slack,
  webhook, whatsapp, discord, msteams, google_chat enabled; email enabled
  but unverified; telegram and telegram_app disabled), with alert bindings
  on fix-api / fix-db / fix-auth mixing `notify_recovery` on/off and
  single/multi-channel bindings.
- **4 maintenance windows** — 1 active (bound to fix-db), 2 upcoming, 1 past.

The script ends with a post-seed verification block that prints Postgres row
counts, per-component last-5-min counters with an expected-vs-actual state
matrix, an HTTP smoke against the public page, the adversarial-title escape
check, and a 90-day ASCII day-strip per component. **Exits non-zero on any
mismatch** — safe to chain in CI.

Env overrides: `SLUG=<org>` (default `devorg`), `RESET_CH=0` to skip
ClickHouse purge if you want to layer additional rows on top of a prior seed
(default `1`).

Then visit:
- Public status page: <http://devorg.lvh.me:8080/>
- Operator dashboard: <http://app.lvh.me:8080/>

## Logging

`docker-compose.yml` sets the default level to:

```
uptimepage=debug,sqlx=warn,hyper=warn,tower_http=info,info
```

For the host workflow, pass it directly:

```bash
RUST_LOG="uptimepage=debug,sqlx=warn" cargo run --bin uptimepage
```

`RUST_LOG` always wins over the config file. Anyhow errors are printed with
`{:#}` from the public-status cache, so the full context chain shows up
without re-running with backtraces.

Stream container logs:

```bash
docker compose logs -f uptimepage
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

## Web UI

The single binary serves both the `/api/v1/*` JSON surface and a
server-rendered HTML UI at `/`. Stack:

- **askama 0.16 + askama_web 0.16** — compile-time HTML templates under
  [`templates/`](../templates/). Type mismatches fail `cargo build`.
- **HTMX 2.0.9 + json-enc** — bundled under [`static/js/`](../static/js/).
  Powers partial swaps (filter, paginate, delete) and JSON form submission.
  No SPA framework.
- **Tailwind CSS 4** — CSS-first config in
  [`static/css/input.css`](../static/css/input.css) (`@source`, `@theme`,
  `@layer components`). No `tailwind.config.js`.
- **ECharts 6** — lazy-loaded from page-level `<script>` tags, only where
  charts exist (dashboard, target detail).

`build.rs` runs `./bin/tailwindcss --minify` before each `cargo build`. First
build fetches the standalone CLI (~30 MB) via `scripts/fetch-tailwind.sh`;
subsequent builds reuse it. After `cargo build --release` you have one
self-contained executable with every template, CSS byte, and vendored JS file
embedded via `rust-embed`.

### Routes

| Path | Owner |
|---|---|
| `GET /` | dashboard (auto-refreshes via HTMX every 5 s) |
| `GET /targets` | targets list + filters |
| `GET /targets/{id}` | target detail with charts and time-range nav |
| `GET /targets/new`, `/targets/{id}/edit` | forms posting JSON to `/api/v1/targets` |
| `GET /web/targets/list` | tbody fragment for filter/paginate swaps |
| `GET /web/partials/dashboard` | chrome-free fragment for the 5 s refresh region |
| `GET /docs` | Swagger UI generated from `/api/openapi.json` |
| `GET /static/*` | embedded assets (`css/`, `js/`, `img/`) |

Every UI mutation hits an existing `/api/v1/*` endpoint — there are no
`/web/*` write routes, which keeps the API the single source of truth and
makes a future SvelteKit port a templates-only rewrite.

### Adding a new page

1. Add a template under `templates/` (extend `base.html`).
2. Add a `#[derive(Template, WebTemplate)]` struct and handler in
   `src/web/views/`.
3. Register the route in `src/web/routes.rs`.
4. Tailwind picks up new utility classes automatically via the
   `@source "../../templates/**/*.html"` directive.

### UI tests

- **Unit (render):** every view in `src/web/views/` ships a `#[test]` that
  renders the template with a fixtures struct and asserts on the output
  (presence of the HTMX hooks, redaction sentinels, table scaffolding).
- **End-to-end:** [`tests/web_e2e_test.rs`](../tests/web_e2e_test.rs) drives
  the merged API+web router via `tower::ServiceExt::oneshot`, covering
  dashboard / list / detail / forms / 404 paths and verifying credential
  redaction never leaks real values into HTML.

```bash
cargo test --lib web::          # unit render tests
cargo test --test web_e2e_test  # e2e
```

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `503 STATUS_DATA_UNAVAILABLE` | Aggregator's first compute failed. Check `uptimepage::public_status::cache` ERROR log for the actual SQL/CH error. |
| `docker compose up --build` takes 5 min on every change | You're on the pre-cargo-chef Dockerfile. Pull latest. |
| Native `cargo run` fails with `Connection refused` | `compose.dev.yml` isn't up, or you forgot to release port 8080 from a running container. |
