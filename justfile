# Common dev workflows. Install: `brew install just` or `cargo install just`.
# Run `just` (no args) to list recipes.

set shell := ["bash", "-cu"]

# The linker lives in .cargo/config.toml (read by `just` AND bare `cargo` AND
# rust-analyzer — one shared build fingerprint, no thrash). Only sccache is an
# env wrapper, and it's set ONLY when present so a vanilla checkout still
# builds (cargo treats an empty RUSTC_WRAPPER as unset). `just setup` installs
# the toolset.
export RUSTC_WRAPPER := `command -v sccache >/dev/null 2>&1 && echo sccache || true`

# Default = list recipes.
default:
    @just --list

# Install the build accelerators: sccache + cargo-nextest, and the fast
# linker (mold on Linux, lld on macOS). Idempotent.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v sccache       >/dev/null 2>&1 || cargo install --locked sccache
    command -v cargo-nextest >/dev/null 2>&1 || cargo install --locked cargo-nextest
    if [ "{{os()}}" = "macos" ]; then
      brew list lld >/dev/null 2>&1 || brew install lld
      cat <<'NOTE'
    macOS lld is opt-in (its brew path is machine-specific, so it can't be
    committed to .cargo/config.toml). For faster local links add to
    ~/.cargo/config.toml:
      [target.aarch64-apple-darwin]
      rustflags = ["-Clink-arg=-fuse-ld=$(brew --prefix lld)/bin/ld64.lld"]
    (substitute the real path; both cargo and rust-analyzer then share it.)
    NOTE
    elif command -v mold >/dev/null 2>&1; then
      :
    elif command -v apt-get >/dev/null 2>&1; then
      sudo apt-get update -q && sudo apt-get install -y mold
    else
      echo "install 'mold' via your package manager (.cargo/config.toml needs it on Linux)"
    fi
    git config core.hooksPath .githooks
    echo "pre-commit installed → .githooks/pre-commit (bypass with --no-verify)"
    echo "setup done — RUSTC_WRAPPER=${RUSTC_WRAPPER:-<none>}; linker via .cargo/config.toml"

# ── Local stack ─────────────────────────────────────────────────────────────

# Bring up postgres + clickhouse only. `cargo run` natively against them.
up:
    docker compose -f compose.dev.yml up -d
    @echo "pg + ch up. run: cargo run --bin uptimepage"

# Bring up the full dev stack incl. uptimepage with live reload.
up-app:
    docker compose -f compose.dev.yml --profile dev-app up -d --build

# Stop everything, keep volumes.
down:
    docker compose -f compose.dev.yml --profile dev-app down

# Stop + wipe DB volumes.
down-clean:
    docker compose -f compose.dev.yml --profile dev-app down -v

# Tail uptimepage logs (works for either dev-app or full docker-compose).
logs:
    docker compose -f compose.dev.yml logs -f uptimepage

# Stand up three real regional agents (regions eu-helsinki, apac-sg, us-east;
# eu-helsinki doubles as the control plane's co-located home region) against the
# running control plane: mints region + agent tokens via the operator API, then
# starts the agent containers.
# Needs the dev stack up (`just up-app` or native `just run`). Idempotent.
dev-regions:
    bash scripts/dev-regions.sh up

# Stop the regional agents, delete their agent + region rows, forget tokens.
dev-regions-down:
    bash scripts/dev-regions.sh down

# Tail the regional agent logs.
dev-regions-logs:
    docker compose -f compose.dev.yml -f compose.dev.agents.yml logs -f agent-eu agent-apac agent-us

# ── Build / run ─────────────────────────────────────────────────────────────

# Native run against `just up`. Debug-level by default for local dev;
# export RUST_LOG to override. Mirrors the dev-app container's filter so
# native and in-container logs match.
run:
    RUST_LOG="${RUST_LOG:-uptimepage=debug,sqlx=warn,hyper=warn,tower_http=info,info}" \
        cargo run --bin uptimepage

# Native run in dashboard mode (brain-only, no in-process probing) — mirrors
# prod. Pair with `just dev-regions` so a real agent covers eu-helsinki;
# otherwise nothing probes. (The dev-app container already runs this mode.)
run-dashboard:
    UPTIMEPAGE_SCHEDULER__ENABLED=false \
    UPTIMEPAGE_SCHEDULER__REGION=eu-helsinki \
    UPTIMEPAGE_SCHEDULER__DEFAULT_REGION=eu-helsinki \
    RUST_LOG="${RUST_LOG:-uptimepage=debug,sqlx=warn,hyper=warn,tower_http=info,info}" \
        cargo run --bin uptimepage

build:
    cargo build --release --bins

# Compile gate — use instead of `cargo check`. `nextest run --no-run` builds
# the test-profile artifacts, so the follow-up `just test` reuses them with
# zero rebuild; `cargo check`'s metadata-only output does not satisfy a test
# build, forcing a full recompile on the next `cargo test`.
check:
    cargo nextest run --workspace --no-run

# Pre-push DB gate: the #[ignore] PG+CH integration tests against a FRESH
# ci_verify database, so edited-in-place migrations are validated like prod's
# remigrate (a stale dev DB hides fresh-schema breaks). Needs the dev stack up
# (`just up`). Classic runner — streams output and skips nextest's build-all
# enumeration stall. ClickHouse defaults to the dev monitor db (tests scope by
# org/target uuids, so the shared volume is fine).
check-db:
    docker exec -i uptimepage-postgres-1 psql -U monitor -d postgres -c "DROP DATABASE IF EXISTS ci_verify WITH (FORCE)"
    docker exec -i uptimepage-postgres-1 psql -U monitor -d postgres -c "CREATE DATABASE ci_verify"
    DATABASE_URL='postgres://monitor:monitor@127.0.0.1:5432/ci_verify' \
    CLICKHOUSE_URL='http://127.0.0.1:8123' \
        cargo test --workspace -- --ignored

# Seed an authenticated owner session (SaaS-mode dev). Prints the cookie +
# a curl snippet. Idempotent; needs the stack up.
dev-login:
    bash scripts/seed-dev-session.sh

# Seed a substantial fixture set: 14 monitors (8 public + 6 internal with
# varied check_spec) + 161 incidents (150 resolved across 87d, 10 active in
# mixed phases, 1 adversarial-title) + 90d ClickHouse history (per-target
# divergent shape, ancient 87-89d outage cluster, 6-day NoData gap on
# fix-email) + 3 notification channels + alert bindings + an active
# maintenance window bound to fix-db. Drives all 5 public component states
# (Operational / Degraded / Partial / Major / Maintenance) plus the
# disabled-target and ungrouped render paths. Idempotent: tagged rows
# wiped before re-insert; CH purged when RESET_CH=1 (default). Ends with a
# post-seed verification block — exits non-zero on any expected-vs-actual
# mismatch. Requires `just dev-login` first so the org exists.
seed-fixtures:
    bash scripts/seed-fixtures.sh

# Seed two monitors for eyeballing the detail latency + breakdown charts:
# `lat-demo` (dense 30d, phase-rich, latency ramp + p95/p99 spikes) and
# `lat-demo-short` (~30min of data — the "new monitor, data < smallest range"
# case). Switching range must re-scale the x-axis and reshape the series.
# Idempotent: tagged rows wiped (PG + CH) before re-insert. Needs `just
# dev-login` first, and a clean CH (`just down-clean && just up-app`) so the
# rollup carries the per-phase columns.
seed-latency-demo:
    bash scripts/seed-latency-demo.sh

# Reset the dev Postgres DB (keeps ClickHouse + the warm build cache).
# Use after editing a migration — pre-launch policy edits migrations in
# place, which trips sqlx's "migration N modified" checksum guard.
db-reset:
    docker compose -f compose.dev.yml exec -T postgres \
        psql -U monitor -d postgres \
        -c "DROP DATABASE IF EXISTS monitor WITH (FORCE);" \
        -c "CREATE DATABASE monitor OWNER monitor;"
    docker compose -f compose.dev.yml restart uptimepage 2>/dev/null || true
    @echo "DB reset. App reconnects + re-applies migrations on a fresh schema."

# ── Tests ───────────────────────────────────────────────────────────────────

# Fast: unit + non-network integration tests, no external services needed.
test:
    cargo test

# All tests including pg- and ch-backed ones. Requires `just up` first.
test-all:
    DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/monitor \
    CLICKHOUSE_URL=http://127.0.0.1:8123 \
        cargo test -- --include-ignored

# Just the CH aggregator integration tests.
test-ch:
    DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/monitor \
    CLICKHOUSE_URL=http://127.0.0.1:8123 \
        cargo test --test clickhouse_aggregator_test -- --ignored

# ── Benchmarks ──────────────────────────────────────────────────────────────

# Uses a throwaway `ci_verify` DB so the dev DB is untouched (harness
# auto-applies migrations on a fresh schema). Needs `just up`. Skips without
# DATABASE_URL so plain `cargo bench`/CI never runs it.
# DB-backed status-page perf benches — run before a release to catch schema/perf drift.
bench-db:
    docker compose -f compose.dev.yml exec -T postgres \
        psql -U monitor -d postgres \
        -c "DROP DATABASE IF EXISTS ci_verify WITH (FORCE);" \
        -c "CREATE DATABASE ci_verify OWNER monitor;"
    docker compose -f compose.dev.yml exec -T clickhouse \
        clickhouse-client -u monitor --password monitor \
        --query "DROP DATABASE IF EXISTS ci_verify"
    docker compose -f compose.dev.yml exec -T clickhouse \
        clickhouse-client -u monitor --password monitor \
        --query "CREATE DATABASE ci_verify"
    DATABASE_URL=postgres://monitor:monitor@127.0.0.1:5432/ci_verify \
    CLICKHOUSE_URL=http://127.0.0.1:8123 \
    CLICKHOUSE_DATABASE=ci_verify \
        cargo bench --bench public_status_ttfb --bench public_status_concurrent

# ── Lints ───────────────────────────────────────────────────────────────────

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings

# Run everything CI runs.
ci: fmt-check clippy test

# Point git at the in-repo hook directory (.githooks/) for this clone.
# Runs cargo fmt --check + scripts/check_tenant_isolation.sh on every commit.
install-hooks:
    git config core.hooksPath .githooks
    @echo "pre-commit installed → .githooks/pre-commit (bypass with --no-verify)"

# ── Database probes ─────────────────────────────────────────────────────────

psql:
    docker compose -f compose.dev.yml exec postgres psql -U monitor -d monitor

clickhouse:
    docker compose -f compose.dev.yml exec clickhouse \
        clickhouse-client -u monitor --password monitor -d monitor

# ── Smoke ───────────────────────────────────────────────────────────────────

# Quick check that the public surface is alive on localhost:8080.
smoke:
    @echo "health:"   ; curl -sS -o /dev/null -w "  %{http_code}\n" http://localhost:8080/healthz
    @echo "ready:"    ; curl -sS -o /dev/null -w "  %{http_code}\n" http://localhost:8080/readyz
    @echo "status:"   ; curl -sS -o /dev/null -w "  %{http_code}\n" http://localhost:8080/api/public/v1/status
    @echo "badge:"    ; curl -sS -o /dev/null -w "  %{http_code}\n" http://localhost:8080/api/public/v1/badge.svg
    @echo "rss:"      ; curl -sS -o /dev/null -w "  %{http_code}\n" http://localhost:8080/api/public/v1/incidents.rss
    @echo "html:"     ; curl -sS -o /dev/null -w "  %{http_code}\n" http://localhost:8080/status

# Smoke-test the operator surface (regions + agents): self-cleaning, asserts
# every status code. Needs the app running with UPTIMEPAGE_OPERATOR__ADMIN_TOKEN
# set; pass the same value as the arg or via OPERATOR_TOKEN.
#   just smoke-operator <admin-token>
smoke-operator token=env_var_or_default("OPERATOR_TOKEN", ""):
    OPERATOR_TOKEN={{token}} bash scripts/smoke-operator.sh
