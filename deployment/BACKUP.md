# Backups & restore

`backup.sh` dumps PostgreSQL + ClickHouse nightly and the Caddy cert volume
weekly, to local disk under `/opt/uptimepage/backups` (mode 0700 — the
dumps contain everything; treat them as secret).

| What | When | Retention | File |
|------|------|-----------|------|
| PostgreSQL (`pg_dump`, gzip) | daily | 14 days | `postgres/monitor-<ts>.sql.gz` |
| ClickHouse (schema + Native data per table, tarred) | daily | 7 days | `clickhouse/monitor-<ts>.tgz` |
| Caddy data volume (certs/keys) | weekly | 28 days | `caddy/caddy-data-<ts>.tgz` |

PostgreSQL is the irreplaceable data (orgs, users, targets, sessions).
ClickHouse is check-result history — large but self-healing as monitors keep
running. Caddy certs re-issue automatically, but the volume backup avoids
hitting Let's Encrypt rate limits on a full rebuild.

## Cron (deploy user)

```
30 3 * * *  /opt/uptimepage/deployment/backup.sh daily   >/dev/null 2>&1
0  4 * * 0  /opt/uptimepage/deployment/backup.sh weekly  >/dev/null 2>&1
```

Activity is appended to `/opt/uptimepage/backups/backup.log`.

## Restore

All commands run from `/opt/uptimepage/deployment` with the stack up.

### PostgreSQL

```bash
gunzip -c /opt/uptimepage/backups/postgres/monitor-<ts>.sql.gz \
  | docker compose exec -T postgres psql -U monitor -d monitor
```

For a clean restore, recreate the database first (stop the app container so
nothing is connected): `docker compose stop uptimepage`, then
`docker compose exec -T postgres psql -U monitor -d postgres -c
'DROP DATABASE monitor; CREATE DATABASE monitor;'`, restore as above,
`docker compose start uptimepage`.

### ClickHouse

```bash
tmp=$(mktemp -d)
tar -C "$tmp" -xzf /opt/uptimepage/backups/clickhouse/monitor-<ts>.tgz
cd "$tmp"/monitor-<ts>
pw=$(grep -E '^CLICKHOUSE_PASSWORD=' /opt/uptimepage/deployment/.env | cut -d= -f2-)
for s in *.schema.sql; do
  t=${s%.schema.sql}
  # .schema.sql holds the original CREATE TABLE; apply it, then load data.
  docker compose -f /opt/uptimepage/deployment/docker-compose.yml exec -T clickhouse \
    clickhouse-client --password "$pw" --query "$(cat "$s")"
  gunzip -c "$t.native.gz" | docker compose -f /opt/uptimepage/deployment/docker-compose.yml \
    exec -T clickhouse clickhouse-client --password "$pw" \
    --query "INSERT INTO monitor.\`$t\` FORMAT Native"
done
```

(The app re-applies ClickHouse migrations on boot, so an empty DB also
self-heals its schema — the data load is the part that matters.)

### Caddy certs

```bash
docker compose stop caddy
docker run --rm -v uptimepage_caddy_data:/data -v \
  /opt/uptimepage/backups/caddy:/backup alpine \
  sh -c 'cd /data && tar xzf /backup/caddy-data-<ts>.tgz'
docker compose start caddy
```

## Off-site (not configured)

Backups are local only — they survive a bad deploy or volume corruption, not
total server loss. Off-site replication (Hetzner Storage Box / S3) is a
follow-up; until then, periodically copy `postgres/` off the box by hand.
