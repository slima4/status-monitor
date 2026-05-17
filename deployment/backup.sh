#!/usr/bin/env bash
# status-monitor backup — PostgreSQL + ClickHouse + Caddy certs, local disk.
#
#   backup.sh daily    pg_dump + ClickHouse dump  (cron: nightly)
#   backup.sh weekly   tar of the caddy_data volume (certs/keys)
#
# Backups land under $BACKUP_ROOT (default /opt/status-monitor/backups),
# mode 0700 (they contain a full DB dump — treat as secret). Retention is
# pruned at the end of each run. Designed to run as the `deploy` user from
# the compose project directory; it shells into the running containers, so
# the stack must be up.
#
# Restore is documented in BACKUP.md (same directory).
set -euo pipefail

ACTION="${1:-}"
COMPOSE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_ROOT="${BACKUP_ROOT:-/opt/status-monitor/backups}"
PG_KEEP_DAYS="${PG_KEEP_DAYS:-14}"
CH_KEEP_DAYS="${CH_KEEP_DAYS:-7}"
CADDY_KEEP_DAYS="${CADDY_KEEP_DAYS:-28}"
PROJECT="status-monitor"            # compose `name:` — used for the volume name
TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOG="$BACKUP_ROOT/backup.log"

cd "$COMPOSE_DIR"
mkdir -p "$BACKUP_ROOT"/{postgres,clickhouse,caddy}
chmod 700 "$BACKUP_ROOT" "$BACKUP_ROOT"/{postgres,clickhouse,caddy}

log() { echo "$(date -u +%FT%TZ) [$ACTION] $*" | tee -a "$LOG" >&2; }
die() { log "ERROR: $*"; exit 1; }

# CLICKHOUSE_PASSWORD is needed to authenticate clickhouse-client; read it
# from .env without echoing. POSTGRES uses the in-container local socket as
# the `monitor` superuser, so no password is needed there.
ch_password() { grep -E '^CLICKHOUSE_PASSWORD=' .env | head -n1 | cut -d= -f2-; }

backup_postgres() {
  local out="$BACKUP_ROOT/postgres/monitor-$TS.sql.gz"
  log "pg_dump → $(basename "$out")"
  docker compose exec -T postgres pg_dump -U monitor --no-owner --no-privileges monitor \
    | gzip -c > "$out"
  [ -s "$out" ] || die "pg dump is empty"
  chmod 600 "$out"
  log "pg_dump ok ($(du -h "$out" | cut -f1))"
}

backup_clickhouse() {
  local dir="$BACKUP_ROOT/clickhouse/monitor-$TS"
  local pw; pw="$(ch_password)"
  [ -n "$pw" ] || die "CLICKHOUSE_PASSWORD not found in .env"
  mkdir -p "$dir"; chmod 700 "$dir"
  # One schema file + one gzipped Native data file per table. Native is the
  # most faithful round-trip format; restore replays CREATE then INSERT.
  local tables
  tables="$(docker compose exec -T clickhouse \
    clickhouse-client --password "$pw" \
    --query "SELECT name FROM system.tables WHERE database='monitor' AND engine NOT LIKE '%View%'")"
  [ -n "$tables" ] || die "no tables found in clickhouse db 'monitor'"
  local t
  while IFS= read -r t; do
    [ -n "$t" ] || continue
    docker compose exec -T clickhouse clickhouse-client --password "$pw" \
      --query "SHOW CREATE TABLE monitor.\`$t\`" > "$dir/$t.schema.sql"
    docker compose exec -T clickhouse clickhouse-client --password "$pw" \
      --query "SELECT * FROM monitor.\`$t\` FORMAT Native" | gzip -c > "$dir/$t.native.gz"
    log "clickhouse table $t ok ($(du -h "$dir/$t.native.gz" | cut -f1))"
  done <<< "$tables"
  tar -C "$BACKUP_ROOT/clickhouse" -czf "$dir.tgz" "$(basename "$dir")"
  rm -rf "$dir"
  chmod 600 "$dir.tgz"
  log "clickhouse ok ($(du -h "$dir.tgz" | cut -f1))"
}

backup_caddy() {
  local vol="${PROJECT}_caddy_data"
  local out="$BACKUP_ROOT/caddy/caddy-data-$TS.tgz"
  docker volume inspect "$vol" >/dev/null 2>&1 || die "volume $vol not found"
  log "tar caddy volume $vol → $(basename "$out")"
  # Throwaway alpine mounts the named volume read-only + the host backup dir.
  docker run --rm \
    -v "$vol":/data:ro \
    -v "$BACKUP_ROOT/caddy":/backup \
    alpine tar -C /data -czf "/backup/$(basename "$out")" .
  [ -s "$out" ] || die "caddy tar is empty"
  chmod 600 "$out"
  log "caddy ok ($(du -h "$out" | cut -f1))"
}

prune() { # dir, days
  find "$1" -type f -name '*.gz' -mtime "+$2" -print -delete | while read -r f; do
    log "pruned $(basename "$f")"
  done
  find "$1" -type f -name '*.tgz' -mtime "+$2" -print -delete | while read -r f; do
    log "pruned $(basename "$f")"
  done
}

case "$ACTION" in
  daily)
    backup_postgres
    backup_clickhouse
    prune "$BACKUP_ROOT/postgres"   "$PG_KEEP_DAYS"
    prune "$BACKUP_ROOT/clickhouse" "$CH_KEEP_DAYS"
    log "daily complete"
    ;;
  weekly)
    backup_caddy
    prune "$BACKUP_ROOT/caddy" "$CADDY_KEEP_DAYS"
    log "weekly complete"
    ;;
  *)
    echo "usage: $0 {daily|weekly}" >&2
    exit 2
    ;;
esac
