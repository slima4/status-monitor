# Common dev workflows. Install: `brew install just` or `cargo install just`.
# Run `just` (no args) to list recipes.

set shell := ["bash", "-cu"]

# Default = list recipes.
default:
    @just --list

# ── Local stack ─────────────────────────────────────────────────────────────

# Bring up postgres + clickhouse only. `cargo run` natively against them.
up:
    docker compose -f compose.dev.yml up -d
    @echo "pg + ch up. run: cargo run --bin status-monitor"

# Bring up the full dev stack incl. status-monitor with live reload.
up-app:
    docker compose -f compose.dev.yml --profile dev-app up -d --build

# Stop everything, keep volumes.
down:
    docker compose -f compose.dev.yml --profile dev-app down

# Stop + wipe DB volumes.
down-clean:
    docker compose -f compose.dev.yml --profile dev-app down -v

# Tail status-monitor logs (works for either dev-app or full docker-compose).
logs:
    docker compose -f compose.dev.yml logs -f status-monitor

# ── Build / run ─────────────────────────────────────────────────────────────

# Native run against `just up`. Debug-level by default for local dev;
# export RUST_LOG to override. Mirrors the dev-app container's filter so
# native and in-container logs match.
run:
    RUST_LOG="${RUST_LOG:-status_monitor=debug,sqlx=warn,hyper=warn,tower_http=info,info}" \
        cargo run --bin status-monitor

build:
    cargo build --release --bins

# Seed an authenticated owner session (SaaS-mode dev). Prints the cookie +
# a curl snippet. Idempotent; needs the stack up.
dev-login:
    bash scripts/seed-dev-session.sh

# Reset the dev Postgres DB (keeps ClickHouse + the warm build cache).
# Use after editing a migration — pre-launch policy edits migrations in
# place, which trips sqlx's "migration N modified" checksum guard.
db-reset:
    docker compose -f compose.dev.yml exec -T postgres \
        psql -U monitor -d postgres \
        -c "DROP DATABASE IF EXISTS monitor WITH (FORCE);" \
        -c "CREATE DATABASE monitor OWNER monitor;"
    docker compose -f compose.dev.yml restart status-monitor 2>/dev/null || true
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
