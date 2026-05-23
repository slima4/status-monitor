#!/usr/bin/env bash
# Seed a substantial fixture set (8 monitors, ~110 incidents across all
# statuses, 90d ClickHouse history) into a running local stack — for UI
# stress-testing, screenshots, and dogfooding the per-org status page with
# realistic volume.
#
# Idempotent: rows are tagged `seed-fixtures` and wiped before re-insert
# so re-running gives the same shape without duplicates. ClickHouse rows
# for the seeded targets are only purged when RESET_CH=1 (ALTER … DELETE
# is async + heavy; on a throwaway stack `just down-clean` is cheaper).
#
# Env overrides:
#   SLUG          org slug to seed onto       (default: devorg)
#   PG_CONTAINER  postgres container name     (default: status-monitor-postgres-1)
#   CH_CONTAINER  clickhouse container name   (default: status-monitor-clickhouse-1)
#   BASE_DOMAIN   for the printed URL         (default: lvh.me)
#   RESET_CH      1 = purge org's CH rows first (default: 0)
#
# Requires `just up-app` + `just dev-login` first (org must already exist).
set -euo pipefail

SLUG="${SLUG:-devorg}"
PG_CONTAINER="${PG_CONTAINER:-status-monitor-postgres-1}"
CH_CONTAINER="${CH_CONTAINER:-status-monitor-clickhouse-1}"
BASE_DOMAIN="${BASE_DOMAIN:-lvh.me}"
RESET_CH="${RESET_CH:-0}"

pg() { docker exec -i "$PG_CONTAINER" psql -U monitor -d monitor -v ON_ERROR_STOP=1 "$@"; }
ch() { docker exec -i "$CH_CONTAINER" clickhouse-client "$@"; }

if ! pg -tAc "SELECT 1 FROM organizations WHERE slug='${SLUG}'" | grep -q 1; then
  echo "error: org '${SLUG}' missing — run 'just dev-login' (or SLUG=… just dev-login) first" >&2
  exit 1
fi

echo "==> Postgres: enable public status + wipe prior fixtures"
pg <<SQL
UPDATE organizations
   SET name                  = 'Fixture Org',
       public_display_name   = 'Fixture Status',
       public_about          = 'Generated fixture data for UI stress-testing.',
       public_brand_color    = '#0ea5e9',
       public_status_enabled = true
 WHERE slug = '${SLUG}';

-- incident_updates cascade via incident FK; wipe is idempotent.
DELETE FROM incidents
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND target_id IN (
     SELECT id FROM targets
      WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
        AND tags @> ARRAY['seed-fixtures']);
DELETE FROM targets
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND tags @> ARRAY['seed-fixtures'];
SQL

echo "==> Postgres: insert 8 monitors (6 visible + 2 internal)"
pg <<SQL
WITH org AS (SELECT id FROM organizations WHERE slug='${SLUG}')
INSERT INTO targets
  (org_id, name, check_spec, interval_secs, enabled, tags,
   public_status, public_name, public_group, public_sort_order)
SELECT org.id, t.name,
       jsonb_build_object(
         'type','http','url',t.url,'method','GET','timeout',5000,
         'follow_redirects',true,'max_redirects',3,
         'expected_status', jsonb_build_object('kind','exact','value',200),
         'headers', '{}'::jsonb, 'verify_tls', true) AS check_spec,
       60, true, ARRAY['seed-fixtures'],
       t.public, t.pname, t.grp, t.so
-- URLs point at well-known stable 200-returners so the dev scheduler
-- produces real `up` check_results on top of the seeded history. Names
-- on the public page stay semantic (API / Website / CDN / …) — the
-- URL is internal to the check spec.
FROM org, (VALUES
  ('fix-api',     'https://api.github.com/',                                              true,  'API',             'Core Services',   0),
  ('fix-web',     'https://www.google.com/',                                              true,  'Website',         'Core Services',   1),
  ('fix-cdn',     'https://cdnjs.cloudflare.com/ajax/libs/jquery/3.7.1/jquery.min.js',    true,  'CDN',             'Core Services',   2),
  ('fix-db',      'https://www.cloudflare.com/cdn-cgi/trace',                             true,  'Database',        'Infrastructure',  0),
  ('fix-auth',    'https://login.microsoftonline.com/common/discovery/v2.0/keys',         true,  'Auth Service',    'Infrastructure',  1),
  ('fix-email',   'https://en.wikipedia.org/wiki/Main_Page',                              true,  'Email Delivery',  'Notifications',   0),
  ('fix-payment', 'https://www.example.com/',                                             false, 'Payment Gateway', 'Internal',        0),
  ('fix-admin',   'https://example.org/',                                                 false, 'Admin Portal',    'Internal',        1)
) AS t(name,url,public,pname,grp,so);
SQL

ORG=$(pg -tAc "SELECT id FROM organizations WHERE slug='${SLUG}';")
read -r T_API T_WEB T_CDN T_DB T_AUTH T_EMAIL T_PAY T_ADMIN < <(pg -tAc \
  "SELECT string_agg(id::text,' ' ORDER BY array_position(
       ARRAY['fix-api','fix-web','fix-cdn','fix-db','fix-auth','fix-email','fix-payment','fix-admin'],
       name))
     FROM targets WHERE org_id='${ORG}' AND tags @> ARRAY['seed-fixtures'];")

# Public targets only — incidents on internal monitors wouldn't surface on
# the public status page anyway. Round-robin across 6 visible targets so the
# load is even.
VISIBLE_TARGETS=("$T_API" "$T_WEB" "$T_CDN" "$T_DB" "$T_AUTH" "$T_EMAIL")

echo "==> Postgres: 100 resolved incidents across 90d (all severities × all status starts × postmortem mix)"
# Layout: 100 rows × generate_series(1..100). Spread starts across ~85 days
# so the archive paginates. Duration 5-185 min keeps the timeline visually
# diverse. Independent modulos decouple severity / status_at_start / title /
# postmortem-or-not so the badge combinations on the public page exercise
# every code path. Every ~5th incident gets a `postmortem` update appended
# AFTER the resolve so the Postmortem phase badge actually appears.
pg <<SQL
WITH s AS (
  SELECT n,
         CASE ((n-1) % 6)
           WHEN 0 THEN '${T_API}'::uuid
           WHEN 1 THEN '${T_WEB}'::uuid
           WHEN 2 THEN '${T_CDN}'::uuid
           WHEN 3 THEN '${T_DB}'::uuid
           WHEN 4 THEN '${T_AUTH}'::uuid
           ELSE        '${T_EMAIL}'::uuid
         END AS target_id,
         (now() - (n * interval '20 hour'))                          AS started_at,
         ((n % 180) + 5) * interval '1 minute'                       AS dur,
         -- Mod 3 / mod 3 / mod 4 are coprime to each other and to mod 6
         -- (target spread), so all 3×3×4 = 36 severity×status×err
         -- combinations appear across the 100-row run.
         (ARRAY['minor','major','critical'])[((n-1) % 3) + 1]        AS sev,
         (ARRAY['down','degraded','error'])[((n*2-1) % 3) + 1]       AS sas,
         (ARRAY['connection refused','timeout','503 service unavailable','dns resolution failed','tls handshake failed','5xx rate breached'])[((n-1) % 6) + 1] AS err,
         -- 12 distinct titles round-robined for archive variety.
         (ARRAY[
           'Elevated 5xx error rate',
           'Slow upstream response',
           'Timeout spike',
           'TCP connection failures',
           'Partial regional outage',
           'TLS handshake failures',
           'DNS resolution failures',
           'Database connection pool exhausted',
           'Memory pressure on edge nodes',
           'CDN cache miss surge',
           'Authentication subsystem degraded',
           'Email delivery queue backlog'
         ])[((n-1) % 12) + 1]                                        AS title,
         -- ~20% of incidents get a postmortem update (n % 5 == 0).
         (n % 5 = 0)                                                 AS with_postmortem
  FROM generate_series(1, 100) n
),
ins AS (
  INSERT INTO incidents
    (org_id, target_id, started_at, ended_at, severity, status_at_start,
     check_count, error_sample, public_title, public_description, duration_secs)
  SELECT '${ORG}'::uuid, s.target_id, s.started_at, s.started_at + s.dur,
         s.sev, s.sas, (s.n % 20) + 5, s.err,
         s.title || ' (fixture #' || s.n || ')',
         'Auto-generated fixture incident #' || s.n
           || '. Severity ' || s.sev || ', start status ' || s.sas
           || ', duration ' || extract(epoch from s.dur)::int || 's.',
         extract(epoch from s.dur)::int
  FROM s
  RETURNING id, started_at, ended_at
)
INSERT INTO incident_updates (org_id, incident_id, posted_at, phase, message)
SELECT '${ORG}', i.id, p.posted_at, p.phase::text, p.message
FROM ins i
JOIN s ON s.started_at = i.started_at
CROSS JOIN LATERAL (
  SELECT i.started_at                                              AS posted_at,
         'investigating'                                           AS phase,
         'Investigating elevated errors.'                          AS message
  UNION ALL SELECT i.started_at + (i.ended_at - i.started_at)*0.25,
         'identified',    'Root cause identified.'
  UNION ALL SELECT i.started_at + (i.ended_at - i.started_at)*0.75,
         'monitoring',    'Mitigation applied; monitoring.'
  UNION ALL SELECT i.ended_at,
         'resolved',      'Service fully restored.'
  UNION ALL SELECT i.ended_at + interval '2 hour',
         'postmortem',    'Postmortem published — see internal docs.'
         WHERE s.with_postmortem
) AS p(posted_at, phase, message);
SQL

echo "==> Postgres: 4 maintenance windows (1 active, 2 upcoming, 1 past)"
pg <<SQL
DELETE FROM maintenance_windows
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND title LIKE 'Fixture %';

INSERT INTO maintenance_windows (org_id, title, description, starts_at, ends_at)
VALUES
  ('${ORG}'::uuid,
   'Fixture rolling database patch',
   'Read-only window while patching primary database.',
   now() - interval '30 minute', now() + interval '90 minute'),
  ('${ORG}'::uuid,
   'Fixture API gateway upgrade',
   'Scheduled upgrade. Brief 503s expected during failover.',
   now() + interval '2 day', now() + interval '2 day 1 hour'),
  ('${ORG}'::uuid,
   'Fixture CDN edge cutover',
   'Cutover to new CDN provider. No expected impact.',
   now() + interval '7 day', now() + interval '7 day 2 hour'),
  ('${ORG}'::uuid,
   'Fixture historical maintenance',
   'Past maintenance retained so the archive view has rows.',
   now() - interval '10 day', now() - interval '10 day' + interval '45 minute');
SQL

echo "==> Postgres: 10 active incidents (5 investigating, 3 identified, 2 monitoring)"
pg <<SQL
WITH s AS (
  SELECT n,
         CASE ((n-1) % 6)
           WHEN 0 THEN '${T_API}'::uuid
           WHEN 1 THEN '${T_WEB}'::uuid
           WHEN 2 THEN '${T_CDN}'::uuid
           WHEN 3 THEN '${T_DB}'::uuid
           WHEN 4 THEN '${T_AUTH}'::uuid
           ELSE        '${T_EMAIL}'::uuid
         END AS target_id,
         now() - (n * interval '13 minute')                         AS started_at,
         -- phase distribution: 1-5 investigating, 6-8 identified, 9-10 monitoring
         CASE WHEN n <= 5 THEN 'investigating'
              WHEN n <= 8 THEN 'identified'
              ELSE             'monitoring' END                     AS current_phase,
         (ARRAY['major','critical','major'])[((n-1) % 3) + 1]       AS sev
  FROM generate_series(1, 10) n
),
ins AS (
  INSERT INTO incidents
    (org_id, target_id, started_at, ended_at, severity, status_at_start,
     check_count, error_sample, public_title, public_description, duration_secs)
  SELECT '${ORG}'::uuid, s.target_id, s.started_at, NULL,
         s.sev, 'down', 3, 'connection refused',
         'Ongoing — ' || initcap(s.current_phase) || ' (fixture #' || s.n || ')',
         'Live fixture incident still in ' || s.current_phase || ' phase.',
         NULL
  FROM s
  RETURNING id, started_at
)
-- One investigating update on every ongoing incident; layer in identified
-- and monitoring rows when the target phase is past those.
INSERT INTO incident_updates (org_id, incident_id, posted_at, phase, message)
SELECT '${ORG}', i.id, p.posted_at, p.phase, p.message
FROM ins i
JOIN s ON s.started_at = i.started_at
CROSS JOIN LATERAL (
  SELECT i.started_at AS posted_at, 'investigating'::text AS phase,
         'Investigating elevated errors.' AS message
  UNION ALL
  SELECT i.started_at + interval '5 minute', 'identified',
         'Root cause identified — failover in progress.'
  WHERE s.current_phase IN ('identified','monitoring')
  UNION ALL
  SELECT i.started_at + interval '10 minute', 'monitoring',
         'Mitigation applied; monitoring recovery.'
  WHERE s.current_phase = 'monitoring'
) AS p;
SQL

if [ "$RESET_CH" = "1" ]; then
  echo "==> ClickHouse: purging existing rows for fixture targets"
  for tid in "${VISIBLE_TARGETS[@]}" "$T_PAY" "$T_ADMIN"; do
    ch -q "ALTER TABLE monitor.check_results DELETE WHERE target_id=toUUID('${tid}') SETTINGS mutations_sync=1"
  done
fi

echo "==> ClickHouse: 90d history per visible monitor (downtime samples at incident windows)"
# Mostly-up baseline + an extra downtime spike every 5d so the uptime sparkline
# isn't a flat green bar. Per-target seed nudges duration_ms so the charts
# differ between monitors.
for tid in "${VISIBLE_TARGETS[@]}"; do
  ch -mn <<SQL
INSERT INTO monitor.check_results (org_id,target_id,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${tid}'),
       now() - toIntervalDay(number) - toIntervalMinute(number % 47),
       'up',
       80 + (cityHash64('${tid}') % 60) + (number % 35),
       200
FROM numbers(90);

INSERT INTO monitor.check_results (org_id,target_id,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${tid}'),
       now() - toIntervalDay((number*5) % 90) - toIntervalMinute(number*3),
       'down', 0, 503, 'connection refused'
FROM numbers(8);

INSERT INTO monitor.check_results (org_id,target_id,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${tid}'),
       now() - toIntervalMinute(number*2),
       'up',
       90 + (cityHash64('${tid}') % 40) + (number % 20),
       200
FROM numbers(30);
SQL
done

echo
echo "Seeded org '${SLUG}' (id ${ORG})."
echo "  monitors  : 8 (6 public + 2 internal)"
echo "  incidents : 110 (100 resolved across 90d + 10 active in mixed phases)"
echo
echo "Public status page: http://${SLUG}.${BASE_DOMAIN}:8080/"
echo "Operator dashboard: http://app.${BASE_DOMAIN}:8080/"
echo "(public page cache TTL ~10s; wait a moment before first load)"
