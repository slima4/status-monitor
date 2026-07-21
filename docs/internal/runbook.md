# Operational Runbook (Tier 1)

> **Private operator document.** Not served by the app, not in the docs
> book. It references real hosts, paths and credentials once filled in —
> keep it out of any public artefact. Replace every `your-server` /
> `your-domain.com` / `<…>` placeholder with the production values before
> first use.

## 1. Incident response procedure

When you suspect production is down or degraded.

### Step 1 — Check status (~2 minutes)

```bash
# Public health check
curl https://app.your-domain.com/healthz                  # expect 200 OK
curl https://app.your-domain.com/readyz                   # expect 200 OK
curl https://acme.your-domain.com/                 # expect 200, HTML

# Server is up?
ssh your-server "uptime"

# Container statuses?
ssh your-server "cd /opt/uptimepage/deployment && docker compose ps"

# Recent logs?
ssh your-server "cd /opt/uptimepage/deployment && docker compose logs --tail=200 uptimepage"
```

### Step 2 — Identify (~5 minutes)

| Symptom | Likely cause | First fix |
|---|---|---|
| `/readyz` returns 503 | DB unreachable | `docker compose ps postgres clickhouse` |
| All endpoints return 502 from Caddy | App down | `docker compose logs uptimepage` then restart |
| Login fails | GitHub OAuth misconfig | check `UPTIMEPAGE_AUTH__GITHUB__CLIENT_ID` |
| Wildcard cert errors | Hetzner DNS token issue | rotate the `HETZNER_DNS_API_TOKEN` |
| Slow responses | DB overload or ClickHouse merge | check `docker stats` |
| Status pages all 404 | App can't reach DB | check connection from container |
| Errors only for one customer | Their org has bad data | inspect the `quota_events` table |

### Step 3 — Communicate

If the outage is longer than 5 minutes and affects multiple users:

- Post on your own (meta) status page if you have one
- Post on your social account
- Wait for resolution before posting "all clear"

### Step 4 — Fix

```bash
# Restart everything
cd /opt/uptimepage/deployment && docker compose restart

# Restart just the app
cd /opt/uptimepage/deployment && docker compose restart uptimepage

# Pull latest image and restart
cd /opt/uptimepage/deployment && docker compose pull && docker compose up -d

# Roll back to a previous image (if the last deploy broke things)
cd /opt/uptimepage/deployment && docker compose pull uptimepage:<previous-tag>
```

### Step 5 — Post-incident

After recovery:

- Note duration, cause and fix in a private incident log
- If user-visible: post the resolution on social/status
- Add monitoring/alerting for the failure mode you missed

## 2. Backup verification

Weekly checklist (set a calendar reminder for Saturday morning).

### Step 1 — Confirm backups exist

```bash
ssh backup-server "ls -lah /backups/uptimepage/ | tail -10"
```

Should show daily files from the past 7+ days. If the newest is older
than 24h the backup job failed — check cron on the backup server.

### Step 2 — Restore to a test environment

```bash
# Provision a throwaway VM (destroy after the test)
# OR use a local VM/container.

# Copy the latest backup
scp backup-server:/backups/uptimepage/postgres-latest.sql.gz test-vm:

# Restore
ssh test-vm "gunzip -c postgres-latest.sql.gz | docker exec -i postgres psql -U monitor monitor"

# Spot-check
ssh test-vm "docker exec postgres psql -U monitor -c 'SELECT COUNT(*) FROM organizations'"
```

### Step 3 — Document

Append a line to `docs/internal/backup-verification.md`:

```
2026-05-15: postgres backup verified. ClickHouse skipped (too large for test).
```

## 3. Key rotation procedures

Annual rotations (set calendar reminders).

### `UPTIMEPAGE_ADMIN_HASH` (basic-auth password)

```bash
# Generate a new hash
docker run --rm caddy:2-alpine caddy hash-password --plaintext "new-password"

# Update .env
ssh your-server "cd /opt/uptimepage/deployment && nano .env"
# Replace UPTIMEPAGE_ADMIN_HASH=...

# Reload Caddy (no downtime)
ssh your-server "cd /opt/uptimepage/deployment && docker compose exec caddy caddy reload --config /etc/caddy/Caddyfile"
```

### `HETZNER_DNS_API_TOKEN`

```bash
# Create a new token at https://dns.hetzner.com/settings/api-token
# Update .env, then restart caddy:
ssh your-server "cd /opt/uptimepage/deployment && docker compose up -d caddy"
# Verify the wildcard cert still works:
curl https://acme.your-domain.com/
# Once verified, revoke the old token in the Hetzner console
```

### GitHub OAuth client secret

Most disruptive — users may need to sign in again.

1. GitHub → Settings → Developer settings → OAuth Apps → uptimepage →
   Generate a new client secret
2. Update `UPTIMEPAGE_AUTH__GITHUB__CLIENT_SECRET` in `.env`
3. Restart the app: `docker compose up -d uptimepage`
4. Test sign-in
5. Delete the old secret in GitHub

### Resend API key

1. Generate a new key at https://resend.com/api-keys
2. Update `[email.resend].api_key` in config
3. Restart the app
4. Send a test invitation to confirm
5. Revoke the old key in Resend

### Database passwords (Postgres, ClickHouse)

More involved — requires brief downtime. Best done during scheduled
maintenance. Write `docs/internal/database-password-rotation.md` when this
is actually needed.

### KEK (credentials encryption key)

**DANGER.** Rotating this without re-encrypting existing credentials makes
all stored target credentials unreadable.

1. Add the new KEK alongside the old one as
   `UPTIMEPAGE_SECURITY__SECONDARY_KEK_BASE64`
2. Deploy a version that decrypts with either key and re-encrypts with the
   new one
3. Wait until all data is re-encrypted (monitor the metric)
4. Remove the old key, restart

Not rotating annually is acceptable unless compromise is suspected. Only
rotate with a clear plan.
