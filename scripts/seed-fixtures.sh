#!/usr/bin/env bash
# Seed a substantial fixture set (14 monitors, ~160 incidents across all
# statuses, 90d ClickHouse history, notification channels, alert bindings,
# maintenance-bound components, adversarial-title incident, four regions +
# per-region agents) into a running local stack — for UI stress-testing,
# screenshots, and smoke-testing every rendered code path on the public +
# operator pages. fix-api probes from 4 regions (quorum 2-of-4), fix-web from 2,
# so the region selectors, per-region breakdown/overlay, quorum detection, and
# the agent dead-man's-switch (one stale agent) all have distinct data.
#
# Coverage matrix on the public page:
#   fix-api    Operational   (mostly-up recent + open incident churn)
#   fix-web    Degraded      (recent writes all 'degraded', no hard fails)
#   fix-cdn    PartialOutage (~30% recent 'down')
#   fix-db     Maintenance   (bound to the active maintenance window)
#   fix-auth   MajorOutage   (>=50% recent 'down')
#   fix-email  Operational + NoData history-gap days (skipped writes)
#   fix-search Operational, no group (renders ungrouped path)
#   fix-paused public + enabled=false (disabled-target render edge case)
# Operator-only:
#   fix-payment http internal, fix-admin http internal,
#   fix-tcp tcp, fix-dns dns, fix-tls tls_cert, fix-domain domain_expiry.
#
# Idempotent: rows are tagged `seed-fixtures` and wiped before re-insert
# so re-running gives the same shape without duplicates. ClickHouse rows
# for the seeded targets are only purged when RESET_CH=1 (ALTER … DELETE
# is async + heavy; on a throwaway stack `just down-clean` is cheaper).
#
# Env overrides:
#   SLUG          org slug to seed onto       (default: devorg)
#   PG_CONTAINER  postgres container name     (default: uptimepage-postgres-1)
#   CH_CONTAINER  clickhouse container name   (default: uptimepage-clickhouse-1)
#   BASE_DOMAIN   for the printed URL         (default: lvh.me)
#   RESET_CH      0 = skip purge (CH rows from prior runs accumulate); the
#                 ClickHouse inserts below are NOT idempotent on timestamp,
#                 so the default is 1 — purge before re-insert. Set to 0
#                 only when you want to layer additional rows on top of an
#                 existing seed. (default: 1)
#
# Requires `just up-app` + `just dev-login` first (org must already exist).
set -euo pipefail

SLUG="${SLUG:-devorg}"
PG_CONTAINER="${PG_CONTAINER:-uptimepage-postgres-1}"
CH_CONTAINER="${CH_CONTAINER:-uptimepage-clickhouse-1}"
BASE_DOMAIN="${BASE_DOMAIN:-lvh.me}"
RESET_CH="${RESET_CH:-1}"

pg() { docker exec -i "$PG_CONTAINER" psql -U monitor -d monitor -v ON_ERROR_STOP=1 "$@"; }
ch() { docker exec -i "$CH_CONTAINER" clickhouse-client "$@"; }

if ! pg -tAc "SELECT 1 FROM organizations WHERE slug='${SLUG}'" | grep -q 1; then
  echo "error: org '${SLUG}' missing — run 'just dev-login' (or SLUG=… just dev-login) first" >&2
  exit 1
fi

echo "==> Postgres: enable public status + wipe prior fixtures"
pg <<SQL
UPDATE organizations SET name = 'Fixture Org' WHERE slug = '${SLUG}';

-- incident_updates + maintenance_window_components cascade via FK; wipe is
-- idempotent. Channels wiped by the same fixture name prefix. Branding and
-- component membership live on a status page (multi-page schema), recreated
-- below — dropping the page cascades its status_page_components.
DELETE FROM incidents
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND target_id IN (
     SELECT id FROM targets
      WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
        AND tags @> ARRAY['seed-fixtures']);
-- Manually-declared fixtures carry no target, so the target-scoped wipe above
-- misses them; clear them by origin. incident_events cascade on delete.
DELETE FROM incidents
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND origin = 'manual';
DELETE FROM targets
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND tags @> ARRAY['seed-fixtures'];
-- target_regions cascade with their targets above; drop every agent in the
-- fixture regions (not just 'Fixture %'-named ones) before the regions, so a
-- stray agent left by another tool can't FK-block the region delete. These
-- regions are seed-owned; 'default' is owned by the app and left intact.
DELETE FROM agents WHERE region IN ('eu-west', 'us-east', 'ap-south');
DELETE FROM regions WHERE id IN ('eu-west', 'us-east', 'ap-south');
DELETE FROM notification_channels
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND name LIKE 'Fixture %';
DELETE FROM status_pages
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND slug IN ('fixtures', '${SLUG}');

-- Page slug must equal the subdomain label (= org slug): the public surface
-- resolves {slug}.{base_domain} against status_pages.slug directly.
INSERT INTO status_pages
  (org_id, slug, name, enabled, public_display_name, public_about, public_brand_color)
SELECT id, '${SLUG}', 'Fixture Status', true, 'Fixture Status',
       'Generated fixture data for UI stress-testing.', '#0ea5e9'
  FROM organizations WHERE slug = '${SLUG}';
SQL

echo "==> Postgres: pick first org member as owner FK for avatars"
OWNER_USER_ID=$(pg -tAc \
  "SELECT m.user_id::text FROM memberships m
     JOIN users u ON u.id = m.user_id AND u.deleted_at IS NULL
    WHERE m.org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
    ORDER BY m.created_at ASC LIMIT 1;" 2>/dev/null || true)
if [[ -z "$OWNER_USER_ID" ]]; then
  echo "    (no membership found — every monitor will render unowned)"
fi

echo "==> Postgres: insert 14 monitors (8 public/visible + 6 internal/varied-spec)"
# Most flags here exist to keep the seed counters from being clobbered by
# the live scheduler — a single real 'down' would flip Operational →
# PartialOutage. fix-search carries NULL group_name + NULL public_group
# so the Ungrouped path renders. group_name is operator-side, distinct
# from public_group, so the two surfaces can diverge.
pg <<SQL
WITH org AS (SELECT id FROM organizations WHERE slug='${SLUG}'),
     sp AS (SELECT id FROM status_pages
             WHERE org_id = (SELECT id FROM org) AND slug = '${SLUG}'),
     spec(name,is_enabled,public,pname,grp,so,gname,has_owner,spec) AS (VALUES
  -- Public / visible HTTP targets (group sort_order drives column layout).
  -- enabled=false on the Operational/Degraded/NoData ones — scheduler stray
  -- checks otherwise corrupt the pure-state seeded counters.
  ('fix-api',     false, true,  'API',             'Core Services',   0,  'API & Web',     true,
   '{"type":"http","url":"https://api.github.com/","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  ('fix-web',     false, true,  'Website',         'Core Services',   1,  'API & Web',     true,
   '{"type":"http","url":"https://www.google.com/","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  ('fix-cdn',     true,  true,  'CDN',             'Core Services',   2,  'CDN',           true,
   '{"type":"http","url":"https://cdnjs.cloudflare.com/ajax/libs/jquery/3.7.1/jquery.min.js","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  ('fix-db',      true,  true,  'Database',        'Infrastructure',  0,  'Infrastructure',true,
   '{"type":"http","url":"https://www.cloudflare.com/cdn-cgi/trace","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  ('fix-auth',    true,  true,  'Auth Service',    'Infrastructure',  1,  'Infrastructure',false,
   '{"type":"http","url":"https://login.microsoftonline.com/common/discovery/v2.0/keys","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  ('fix-email',   false, true,  'Email Delivery',  'Notifications',   0,  'Notifications', true,
   '{"type":"http","url":"https://en.wikipedia.org/wiki/Main_Page","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  -- Public component, NULL public_group AND NULL group_name → exercises
  -- the "Ungrouped" path on BOTH the public + Monitors pages.
  ('fix-search',  false, true,  'Search',          NULL,              0,  NULL,            false,
   '{"type":"http","url":"https://duckduckgo.com/","method":"HEAD","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"range","value":{"min":200,"max":399}},"headers":{},"verify_tls":true}'),
  -- Public component, enabled=false → renders disabled marker on operator
  -- page; still surfaces on the public page (filter is public_status only).
  ('fix-paused',  false, true,  'Beta Sandbox',    'Beta',            0,  'Beta',          true,
   '{"type":"http","url":"https://httpbin.org/status/200","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  -- Internal HTTP targets (legacy fixture rows; not on public page).
  ('fix-payment', true,  false, 'Payment Gateway', 'Internal',        0,  'Integrations',  true,
   '{"type":"http","url":"https://www.example.com/","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  ('fix-admin',   true,  false, 'Admin Portal',    'Internal',        1,  'Integrations',  false,
   '{"type":"http","url":"https://example.org/","method":"GET","timeout":5000,"follow_redirects":true,"max_redirects":3,"expected_status":{"kind":"exact","value":200},"headers":{},"verify_tls":true}'),
  -- Non-HTTP check_spec variants — only the operator targets UI renders these.
  ('fix-tcp',     true,  false, 'Postgres Socket', 'Internal',        2,  'Infrastructure',true,
   '{"type":"tcp","host":"db.example.com","port":5432,"timeout":3000}'),
  ('fix-dns',     true,  false, 'DNS Apex',        'Internal',        3,  'Infrastructure',false,
   '{"type":"dns","domain":"example.com","record_type":"A","resolver":"1.1.1.1","expected_contains":"93.184","timeout":3000}'),
  ('fix-tls',     true,  false, 'TLS Cert',        'Internal',        4,  'Infrastructure',false,
   '{"type":"tls_cert","host":"example.com","port":443,"server_name":null,"warn_days":30,"critical_days":7,"timeout":5000}'),
  ('fix-domain',  true,  false, 'Domain Expiry',   'Internal',        5,  'Infrastructure',false,
   '{"type":"domain_expiry","domain":"example.com","warn_days":60,"critical_days":14,"timeout":10000}')
     ),
     ins AS (
       INSERT INTO targets
         (org_id, name, check_spec, interval_secs, enabled, tags, group_name, owner_user_id)
       SELECT org.id, spec.name, spec.spec::jsonb,
              CASE WHEN spec.spec::jsonb->>'type' IN ('tls_cert','domain_expiry') THEN 86400 ELSE 60 END,
              spec.is_enabled, ARRAY['seed-fixtures'], spec.gname,
              CASE WHEN spec.has_owner AND '${OWNER_USER_ID}' <> ''
                   THEN '${OWNER_USER_ID}'::uuid ELSE NULL END
       FROM org, spec
       RETURNING id, name
     )
-- Public monitors become components of the fixture status page; the rest stay
-- operator-only (and now still get incidents via the broadened writer).
INSERT INTO status_page_components
  (org_id, status_page_id, target_id, public_name, public_group, sort_order)
SELECT (SELECT id FROM org), (SELECT id FROM sp), ins.id, spec.pname, spec.grp, spec.so
FROM ins JOIN spec ON spec.name = ins.name
WHERE spec.public;

-- Demonstrate the managed-by badge: pretend the Integrations monitors are
-- authored by Terraform and one by a raw API token. UI-authored rows (the
-- default 'ui') stay chip-less.
UPDATE targets SET write_source = 'terraform'
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND tags @> ARRAY['seed-fixtures'] AND name IN ('fix-payment','fix-admin');
UPDATE targets SET write_source = 'api'
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND tags @> ARRAY['seed-fixtures'] AND name = 'fix-tcp';
SQL

ORG=$(pg -tAc "SELECT id FROM organizations WHERE slug='${SLUG}';")
: "${ORG:?org id query returned empty — preflight check should have caught this}"
read -r T_API T_WEB T_CDN T_DB T_AUTH T_EMAIL T_SEARCH T_PAUSED \
        T_PAY T_ADMIN T_TCP T_DNS T_TLS T_DOMAIN < <(pg -tAc \
  "SELECT string_agg(id::text,' ' ORDER BY array_position(
       ARRAY['fix-api','fix-web','fix-cdn','fix-db','fix-auth','fix-email',
             'fix-search','fix-paused',
             'fix-payment','fix-admin','fix-tcp','fix-dns','fix-tls','fix-domain'],
       name))
     FROM targets WHERE org_id='${ORG}' AND tags @> ARRAY['seed-fixtures'];")

# Guard against silent `read -r` misalignment: if any target name in the
# VALUES block above drifts from the ORDER BY array, string_agg sorts the
# unknown name LAST (array_position → NULL → NULLS LAST), shifting every
# positional assignment by one. Empty strings then become `''::uuid` cast
# errors many lines into the script. Validate up-front.
for _v in T_API T_WEB T_CDN T_DB T_AUTH T_EMAIL T_SEARCH T_PAUSED \
          T_PAY T_ADMIN T_TCP T_DNS T_TLS T_DOMAIN; do
  _id="${!_v}"
  if [[ ${#_id} -ne 36 ]]; then
    echo "error: ${_v}='${_id}' — expected 36-char UUID. Target VALUES list and ORDER BY array out of sync?" >&2
    exit 1
  fi
done
unset _v _id

echo "==> Postgres: regions, multi-region assignments, quorum policy, operator agents"
# Four regions so the dashboard/monitor region selectors, quorum detection, and
# the operator agent surface all have real data. Every fixture target runs on
# 'default' (the control plane); fix-api additionally probes from all three
# remote regions and fix-web from eu-west. Agent rows drive the operator surface
# (credentials are non-verifying placeholders, display only).
pg <<SQL
INSERT INTO regions (id, name, location) VALUES
  ('default', 'Default', 'control plane'),
  ('eu-west', 'EU West', 'Amsterdam'),
  ('us-east', 'US East', 'Virginia'),
  ('ap-south', 'AP South', 'Singapore')
ON CONFLICT (id) DO NOTHING;

INSERT INTO target_regions (target_id, region)
SELECT id, 'default' FROM targets
 WHERE org_id = '${ORG}' AND tags @> ARRAY['seed-fixtures']
ON CONFLICT DO NOTHING;

-- fix-api → all three remote regions (4 total); fix-web → eu-west (2 total).
INSERT INTO target_regions (target_id, region)
SELECT id, r FROM targets, unnest(ARRAY['eu-west','us-east','ap-south']) AS r
 WHERE org_id = '${ORG}' AND tags @> ARRAY['seed-fixtures'] AND name = 'fix-api'
ON CONFLICT DO NOTHING;

INSERT INTO target_regions (target_id, region)
SELECT id, 'eu-west' FROM targets
 WHERE org_id = '${ORG}' AND tags @> ARRAY['seed-fixtures'] AND name = 'fix-web'
ON CONFLICT DO NOTHING;

-- Multi-region detection, exercising both shapes: fix-api a fixed count (2 of 4),
-- fix-web the symbolic "all" (both of 2).
UPDATE targets SET region_policy = '{"count":2}'::jsonb
 WHERE org_id = '${ORG}' AND tags @> ARRAY['seed-fixtures'] AND name = 'fix-api';
UPDATE targets SET region_policy = '"all"'::jsonb
 WHERE org_id = '${ORG}' AND tags @> ARRAY['seed-fixtures'] AND name = 'fix-web';

-- A fresh probe per remote region plus a deliberately STALE one (last seen
-- 10 min ago) so the dead-man's-switch (stale flag + agent_up gauge) shows.
INSERT INTO agents (region, name, enabled, token_hash, token_prefix, last_seen_at) VALUES
  ('eu-west',  'Fixture EU probe',         true, 'seed-fixture-cred-eu', 'sm_agent_fixeu', now()),
  ('us-east',  'Fixture US probe',         true, 'seed-fixture-cred-us', 'sm_agent_fixus', now()),
  ('ap-south', 'Fixture AP probe (stale)', true, 'seed-fixture-cred-ap', 'sm_agent_fixap', now() - interval '10 minutes');
SQL

# Public targets only — incidents on internal monitors wouldn't surface on
# the public status page anyway. Round-robin across 6 visible HTTP targets
# so the load is even. fix-search/fix-paused get their own targeted rows
# below; non-HTTP targets don't appear here.
VISIBLE_TARGETS=("$T_API" "$T_WEB" "$T_CDN" "$T_DB" "$T_AUTH" "$T_EMAIL")

echo "==> Postgres: 150 resolved incidents across 87d (all severities × all status starts × postmortem mix)"
# Layout: 150 rows × generate_series(1..150). 14h interval packs ~60 starts
# into the last 30 days (clears the page's 50-incident render cap → the
# "Older incidents →" link appears) while still spanning ~87 days so the
# archive paginates. Duration 5-185 min keeps the timeline visually diverse.
# Independent modulos decouple severity / status_at_start / title /
# postmortem-or-not so the badge combinations on the public page exercise
# every code path. Every ~5th incident gets a postmortem update appended
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
         (now() - (n * interval '14 hour'))                          AS started_at,
         ((n % 180) + 5) * interval '1 minute'                       AS dur,
         -- Mod 3 / mod 3 / mod 4 are coprime to each other and to mod 6
         -- (target spread), so all 3×3×4 = 36 severity×status×err
         -- combinations appear across the 150-row run.
         (ARRAY['minor','major','critical'])[((n-1) % 3) + 1]        AS sev,
         (ARRAY['down','degraded','error'])[((n*2-1) % 3) + 1]       AS sas,
         -- Phase-specific reasons the probe now emits (TCP / DNS / TLS); the
         -- detail view maps "dns: …" to friendlier copy, the rest pass through.
         (ARRAY['connection refused','connect timeout','dns: domain not found','certificate expired','certificate not trusted','no response'])[((n-1) % 6) + 1] AS err,
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
  FROM generate_series(1, 150) n
),
ins AS (
  INSERT INTO incidents
    (org_id, target_id, started_at, ended_at, severity, status_at_start,
     check_count, error_sample, public_title, public_description, duration_secs,
     state, visibility, origin)
  SELECT '${ORG}'::uuid, s.target_id, s.started_at, s.started_at + s.dur,
         s.sev, s.sas, (s.n % 20) + 5, s.err,
         s.title || ' (fixture #' || s.n || ')',
         'Auto-generated fixture incident #' || s.n
           || '. Severity ' || s.sev || ', start status ' || s.sas
           || ', duration ' || extract(epoch from s.dur)::int || 's.',
         extract(epoch from s.dur)::int,
         -- Closed, public, monitor-opened: operational state mirrors ended_at.
         'resolved', 'public', 'monitor'
  FROM s
  RETURNING id, started_at, ended_at
)
INSERT INTO incident_updates (org_id, incident_id, posted_at, phase, message, author)
SELECT '${ORG}', i.id, p.posted_at, p.phase::text, p.message, NULLIF('${OWNER_USER_ID}', '')
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

echo "==> Postgres: 4 maintenance windows (1 active, 2 upcoming, 1 past) + bind active → fix-db"
# Binding the *active* window to fix-db flips that component's
# current_status to Maintenance (component_status() short-circuits on
# maintenance_active regardless of recent up/down counters).
pg <<SQL
DELETE FROM maintenance_windows
 WHERE org_id = (SELECT id FROM organizations WHERE slug='${SLUG}')
   AND title LIKE 'Fixture %';

WITH ins AS (
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
     now() - interval '10 day', now() - interval '10 day' + interval '45 minute')
  RETURNING id, title
)
INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
SELECT '${ORG}'::uuid, ins.id, '${T_DB}'::uuid
FROM ins WHERE ins.title = 'Fixture rolling database patch';
SQL

echo "==> Postgres: 4 active incidents (one per frozen public monitor; all phases + states)"
# One OPEN incident per target — mirrors the writer's single-open-per-target
# invariant (insert_open guards NOT EXISTS open), so the public active banner
# never lists the same component twice. Seeded only on enabled=false public
# monitors: the live writer never checks disabled targets, so it can't
# auto-resolve these mid-demo. Excludes fix-db (Maintenance-bound) and the
# enabled=true fix-cdn / fix-auth, whose incidents the live writer owns.
pg <<SQL
WITH s AS (
  SELECT target_id, phase, sev,
         now() - (ord * interval '13 minute') AS started_at
  FROM (VALUES
    ('${T_API}'::uuid,    'investigating', 'major',    1),
    ('${T_WEB}'::uuid,    'identified',    'critical', 2),
    ('${T_EMAIL}'::uuid,  'monitoring',    'major',    3),
    ('${T_SEARCH}'::uuid, 'investigating', 'minor',    4)
  ) AS v(target_id, phase, sev, ord)
),
ins AS (
  INSERT INTO incidents
    (org_id, target_id, started_at, ended_at, severity, status_at_start,
     check_count, error_sample, public_title, public_description, duration_secs,
     state, visibility, origin, acknowledged_at, acknowledged_by)
  SELECT '${ORG}'::uuid, s.target_id, s.started_at, NULL,
         s.sev, 'down', 3, 'connection refused',
         'Ongoing — ' || initcap(s.phase) || ' (' || s.phase || ' phase)',
         'Live fixture incident still in ' || s.phase || ' phase.',
         NULL,
         -- Past-investigating phases are acknowledged by the on-call owner;
         -- still-investigating ones are unacknowledged (triggered).
         CASE WHEN s.phase = 'investigating' THEN 'triggered' ELSE 'acknowledged' END,
         'public', 'monitor',
         CASE WHEN s.phase <> 'investigating'
              THEN s.started_at + interval '4 minute' ELSE NULL END,
         CASE WHEN s.phase <> 'investigating' AND '${OWNER_USER_ID}' <> ''
              THEN '${OWNER_USER_ID}'::uuid ELSE NULL END
  FROM s
  RETURNING id, started_at
)
-- One investigating update on every ongoing incident; layer in identified
-- and monitoring rows when the target phase is past those.
INSERT INTO incident_updates (org_id, incident_id, posted_at, phase, message, author)
SELECT '${ORG}', i.id, p.posted_at, p.phase, p.message, NULLIF('${OWNER_USER_ID}', '')
FROM ins i
JOIN s ON s.started_at = i.started_at
CROSS JOIN LATERAL (
  SELECT i.started_at AS posted_at, 'investigating'::text AS phase,
         'Investigating elevated errors.' AS message
  UNION ALL
  SELECT i.started_at + interval '5 minute', 'identified',
         'Root cause identified — failover in progress.'
  WHERE s.phase IN ('identified','monitoring')
  UNION ALL
  SELECT i.started_at + interval '10 minute', 'monitoring',
         'Mitigation applied; monitoring recovery.'
  WHERE s.phase = 'monitoring'
) AS p;
SQL

echo "==> Postgres: operational-layer incidents (internal-only + manually declared) with activity timelines"
# These exercise the operator incidents console specifically:
#   * internal incidents on non-public monitors (visibility='internal') — they
#     appear in the console but NEVER on the public status page, proving the
#     writer can open incidents for any monitor without leaking it publicly.
#   * a manually declared incident (origin='manual', no monitor) acknowledged
#     by the on-call owner.
# Each gets incident_events so the detail page's Activity timeline is populated.
pg <<SQL
WITH ins AS (
  INSERT INTO incidents
    (org_id, target_id, started_at, ended_at, severity, status_at_start,
     check_count, error_sample, title, duration_secs, state, visibility, origin)
  VALUES
    ('${ORG}'::uuid, '${T_PAY}'::uuid, now() - interval '40 minute', NULL,
     'critical', 'down', 4, 'connection refused',
     'Payment gateway unreachable', NULL, 'triggered', 'internal', 'monitor'),
    ('${ORG}'::uuid, '${T_ADMIN}'::uuid, now() - interval '3 hour', now() - interval '2 hour',
     'major', 'error', 6, 'http 500',
     'Admin portal 5xx spike', 3600, 'resolved', 'internal', 'monitor')
  RETURNING id, started_at, state
)
INSERT INTO incident_events (org_id, incident_id, occurred_at, kind, actor_type, actor_id, message)
SELECT '${ORG}'::uuid, i.id, e.occurred_at, e.kind, e.actor_type,
       CASE WHEN e.actor_type = 'user' AND '${OWNER_USER_ID}' <> ''
            THEN '${OWNER_USER_ID}'::uuid ELSE NULL END,
       e.message
FROM ins i
CROSS JOIN LATERAL (
  SELECT i.started_at AS occurred_at, 'triggered'::text AS kind,
         'system'::text AS actor_type, NULL::text AS message
  UNION ALL SELECT i.started_at + interval '6 minute', 'acknowledged', 'user', 'Taking a look.'
    WHERE i.state <> 'triggered'
  UNION ALL SELECT i.started_at + interval '55 minute', 'resolved', 'user', 'Rolled back the bad deploy.'
    WHERE i.state = 'resolved'
) AS e(occurred_at, kind, actor_type, message);

WITH ins AS (
  INSERT INTO incidents
    (org_id, target_id, started_at, ended_at, severity, status_at_start,
     check_count, title, duration_secs, state, visibility, origin,
     acknowledged_at, acknowledged_by)
  VALUES
    ('${ORG}'::uuid, NULL, now() - interval '20 minute', NULL,
     'major', 'down', 0, 'Customer-reported checkout failures', NULL,
     'acknowledged', 'internal', 'manual', now() - interval '15 minute',
     CASE WHEN '${OWNER_USER_ID}' <> '' THEN '${OWNER_USER_ID}'::uuid ELSE NULL END)
  RETURNING id, started_at
)
INSERT INTO incident_events (org_id, incident_id, occurred_at, kind, actor_type, actor_id, message)
SELECT '${ORG}'::uuid, i.id, e.occurred_at, e.kind, e.actor_type,
       CASE WHEN '${OWNER_USER_ID}' <> '' THEN '${OWNER_USER_ID}'::uuid ELSE NULL END,
       e.message
FROM ins i
CROSS JOIN LATERAL (
  SELECT i.started_at AS occurred_at, 'triggered'::text AS kind,
         'user'::text AS actor_type, 'Declared from support ticket #4821.'::text AS message
  UNION ALL SELECT i.started_at + interval '5 minute', 'acknowledged', 'user', 'On call investigating.'
  UNION ALL SELECT i.started_at + interval '8 minute', 'note', 'user',
         'Correlated with the payment gateway incident.'
) AS e(occurred_at, kind, actor_type, message);
SQL

echo "==> Postgres: 9 notification channels (one per transport) and alert bindings"
# Channel kinds match ChannelConfig — one per variant. The BYO Telegram row is
# disabled so the operator UI renders both the enabled and disabled states;
# the linked telegram_app row carries the platform-disable note.
# Bindings are pure delivery targets ({channel_id}); the firing policy
# (alert_confirmations, notify_recovery) lives on the monitor and is set below.
pg <<SQL
INSERT INTO notification_channels (org_id, name, kind, config, external_ref, enabled, disabled_reason) VALUES
  ('${ORG}'::uuid, 'Fixture Slack',    'slack',
   '{"type":"slack","webhook_url":"https://hooks.slack.com/services/T0000/B0000/XXXXXXXXXXXXXXXXXXXXXXXX"}'::jsonb,
   NULL, true, NULL),
  ('${ORG}'::uuid, 'Fixture Webhook',  'webhook',
   '{"type":"webhook","url":"https://example.com/hook","headers":{"X-Fixture":"1"}}'::jsonb,
   NULL, true, NULL),
  ('${ORG}'::uuid, 'Fixture Telegram', 'telegram',
   '{"type":"telegram","bot_token":"1234567890:AAH-fixture-bot-token","chat_id":"-1001234567890"}'::jsonb,
   NULL, false, NULL),
  ('${ORG}'::uuid, 'Fixture Tg Linked', 'telegram_app',
   '{"type":"telegram_app","chat_id":"-1009876543210","chat_title":"Fixture Ops"}'::jsonb,
   '-1009876543210', false, 'unlinked from the Telegram side'),
  ('${ORG}'::uuid, 'Fixture WhatsApp', 'whatsapp',
   '{"type":"whatsapp","access_token":"EAAG-fixture-token","phone_number_id":"106540352242922","to":"15551234567","template_name":"uptime_alert"}'::jsonb,
   NULL, true, NULL),
  ('${ORG}'::uuid, 'Fixture Discord', 'discord',
   '{"type":"discord","webhook_url":"https://discord.com/api/webhooks/000000000000000000/fixture-discord-token"}'::jsonb,
   NULL, true, NULL),
  ('${ORG}'::uuid, 'Fixture Teams', 'msteams',
   '{"type":"msteams","webhook_url":"https://prod-00.westus.logic.azure.com/workflows/fixture/triggers/manual/paths/invoke"}'::jsonb,
   NULL, true, NULL),
  ('${ORG}'::uuid, 'Fixture GChat', 'google_chat',
   '{"type":"google_chat","webhook_url":"https://chat.googleapis.com/v1/spaces/AAAA0000/messages?key=fixture-key&token=fixture-token"}'::jsonb,
   NULL, true, NULL),
  ('${ORG}'::uuid, 'Fixture Email', 'email',
   '{"type":"email","to":"oncall@example.com"}'::jsonb,
   NULL, true, NULL);

-- The email fixture renders the unverified chip; engine deliveries to it
-- record failures, which is the state worth eyeballing.


-- Managed-by badge on the channels list too: webhook as Terraform, Slack as
-- a raw API token; Telegram stays UI-authored (no chip).
UPDATE notification_channels SET write_source = 'terraform'
 WHERE org_id='${ORG}'::uuid AND name = 'Fixture Webhook';
UPDATE notification_channels SET write_source = 'api'
 WHERE org_id='${ORG}'::uuid AND name = 'Fixture Slack';

-- Three monitors, each a different notification shape — and a different reminder
-- cadence so the form's renotify dropdown renders hourly / off / 15-minute:
--   fix-api  : both enabled channels, 3 confirmations, recovery on,  remind hourly
--   fix-db   : Slack only,            5 confirmations, recovery off,  reminders off
--   fix-auth : Slack + Telegram,      2 confirmations, recovery on,   remind every 15m
-- COALESCE guards against a future rename / typo in the WHERE clause:
-- jsonb_agg over zero rows returns NULL, which violates alerts NOT NULL.
UPDATE targets SET
  alerts = COALESCE((
    SELECT jsonb_agg(jsonb_build_object('channel_id', id))
      FROM notification_channels
     WHERE org_id='${ORG}'::uuid AND name IN ('Fixture Slack','Fixture Webhook')
  ), '[]'::jsonb),
  alert_confirmations = 3, notify_recovery = true, renotify_interval_secs = 3600
WHERE id='${T_API}'::uuid;

UPDATE targets SET
  alerts = COALESCE((
    SELECT jsonb_agg(jsonb_build_object('channel_id', id))
      FROM notification_channels
     WHERE org_id='${ORG}'::uuid AND name = 'Fixture Slack'
  ), '[]'::jsonb),
  alert_confirmations = 5, notify_recovery = false, renotify_interval_secs = 0
WHERE id='${T_DB}'::uuid;

UPDATE targets SET
  alerts = COALESCE((
    SELECT jsonb_agg(jsonb_build_object('channel_id', id))
      FROM notification_channels
     WHERE org_id='${ORG}'::uuid AND name IN ('Fixture Slack','Fixture Telegram')
  ), '[]'::jsonb),
  alert_confirmations = 2, notify_recovery = true, renotify_interval_secs = 900
WHERE id='${T_AUTH}'::uuid;
SQL

echo "==> Postgres: 1 adversarial-title incident (XSS / day-popover JSON-escape smoke)"
# Title carries <, >, &, ", </script>, <!--, control bytes — covers the
# escape path that emits the inline #day-strip-data JSON blob safe-by-default.
pg <<SQL
INSERT INTO incidents
  (org_id, target_id, started_at, ended_at, severity, status_at_start,
   check_count, error_sample, public_title, public_description, duration_secs,
   state, visibility, origin)
VALUES
  ('${ORG}'::uuid, '${T_SEARCH}'::uuid,
   now() - interval '36 hour', now() - interval '35 hour',
   'minor', 'degraded', 7, 'parse error',
   \$ADV\$Adversarial title </script><!-- & "double" — fixture smoke\$ADV\$,
   \$ADV\$Tests JSON escaping for the day_strip popover blob. Contains < > & " ' </script> <!-- and a unicode em-dash —.\$ADV\$,
   3600, 'resolved', 'public', 'monitor');

INSERT INTO incident_updates (org_id, incident_id, posted_at, phase, message, author)
SELECT '${ORG}'::uuid, i.id, p.posted_at, p.phase, p.message, NULLIF('${OWNER_USER_ID}', '')
FROM (SELECT id, started_at, ended_at FROM incidents
       WHERE org_id='${ORG}'::uuid AND target_id='${T_SEARCH}'::uuid
         AND public_title LIKE 'Adversarial title%') i
CROSS JOIN LATERAL (
  SELECT i.started_at AS posted_at, 'investigating'::text AS phase,
         'Investigating <script>alert(1)</script> — content escaped client-side.' AS message
  UNION ALL SELECT i.ended_at, 'resolved', 'Resolved & safe.'
) p;
SQL

# Mark the seeded 'triggered' incidents as already-paged. Two background sweeps
# act on a triggered incident otherwise: reconcile re-pages one with no
# notification row at all, and renotify re-pages one whose last page is older
# than the monitor's reminder interval. Synthesize one delivered 'opened'
# notification each, stamped now() (not started_at) so the reminder is a full
# interval out and neither sweep churns on boot. Attributed to the webhook
# channel so the incident's Delivery section shows a channel name. Cascades on
# incident delete, so re-runs stay clean.
pg <<SQL
INSERT INTO incident_notifications (org_id, incident_id, channel_id, transport, reason, status, attempt, sent_at)
SELECT i.org_id, i.id,
       (SELECT id FROM notification_channels WHERE org_id = i.org_id AND kind = 'webhook' LIMIT 1),
       'webhook', 'opened', 'sent', 1, now()
FROM incidents i
WHERE i.org_id = '${ORG}'::uuid AND i.state = 'triggered';

-- Delivery-log coverage: give one open incident a retrying page and a
-- dead-lettered one too, so the incident Delivery section renders all of
-- sent / retrying / dead-letter at once. next_attempt_at sits an hour out (or
-- NULL for the dead-letter) so the retry sweep stays quiet on a fixture box.
INSERT INTO incident_notifications
  (org_id, incident_id, channel_id, transport, reason, status, attempt, error, next_attempt_at)
SELECT i.org_id, i.id,
       (SELECT id FROM notification_channels WHERE org_id = i.org_id AND kind = 'slack' LIMIT 1),
       'slack', 'opened', v.status, v.attempt, v.error, v.next_attempt_at
FROM incidents i
CROSS JOIN (VALUES
  ('failed'::text, 3, 'connection timed out', now() + interval '1 hour'),
  ('failed'::text, 5, 'connection refused',   NULL::timestamptz)
) AS v(status, attempt, error, next_attempt_at)
WHERE i.org_id = '${ORG}'::uuid AND i.target_id = '${T_PAY}'::uuid AND i.state = 'triggered';
SQL

if [ "$RESET_CH" = "1" ]; then
  echo "==> ClickHouse: purging existing rows for fixture targets"
  # Every tid below was length-validated post-`read -r`; an empty tid here
  # would build `toUUID('')` and either abort the loop mid-purge (modern CH)
  # or — on older CH — purge the zero-UUID partition.
  for tid in "${VISIBLE_TARGETS[@]}" "$T_SEARCH" "$T_PAUSED" "$T_PAY" "$T_ADMIN" \
             "$T_TCP" "$T_DNS" "$T_TLS" "$T_DOMAIN"; do
    ch -q "ALTER TABLE monitor.check_results DELETE WHERE target_id=toUUID('${tid}') SETTINGS mutations_sync=1"
  done
fi

echo "==> ClickHouse: 90d baseline history for visible monitors (per-target divergent shape)"
# Mostly-up baseline + outage spikes + a few degraded days. Every row is
# shifted 6h into the past so day_index=0 rows can't leak into the
# last-5-min classifier window. Shape diverges per target via cityHash64(tid):
#   * outage count    : 4..15  (uptime% then ranges ~83%..96% per target)
#   * outage day-set  : prime-stride walk so two targets rarely share a day
#   * degraded count  : 1..5   (rendered as PartialOutage cells — the day-
#                              strip MV doesn't preserve down/degraded yet,
#                              so degraded *days* collapse into the outage
#                              count; component-level Degraded state is
#                              still surfaced via fix-web's last-5-min mix)
#   * latency band    : 80..200 ms baseline so per-target sparkline differs
# Plus an explicit 87-89d "ancient outage" cluster on the first 3 visible
# monitors so the leftmost day-strip cells have a guaranteed downtime marker
# (otherwise the prime-stride walk rarely lands deep into the 90d window).
# fix-email is handled separately so we can punch a NoData gap into it.
for tid in "$T_API" "$T_WEB" "$T_CDN" "$T_DB" "$T_AUTH" "$T_SEARCH"; do
  ch -mn <<SQL
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${tid}'),'default',
       now() - toIntervalHour(6) - toIntervalDay(number) - toIntervalMinute(number % 47),
       'up',
       80 + (cityHash64('${tid}') % 120) + (number % 35),
       200
FROM numbers(90);

-- Down spikes pinned to days 0..59 so they never collide with degraded days
-- (60..86) — keeps the day_state classifier from masking Degraded under
-- PartialOutage when both types land on the same calendar day.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${tid}'),'default',
       now() - toIntervalHour(6)
             - toIntervalDay((number * 7 + (cityHash64('${tid}') % 13)) % 60)
             - toIntervalMinute(((number * 11 + cityHash64('${tid}')) % 60)),
       'down', 0, 503, 'connection refused'
FROM numbers(20)
WHERE number < (4 + (cityHash64('${tid}') % 12));

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${tid}'),'default',
       now() - toIntervalHour(6)
             - toIntervalDay(60 + (number * 3 + (cityHash64('${tid}') % 7)) % 27)
             - toIntervalMinute(((number * 23 + cityHash64('${tid}')) % 60)),
       'degraded',
       600 + (cityHash64('${tid}') % 400) + (number * 40),
       200
FROM numbers(8)
WHERE number < (1 + (bitShiftRight(cityHash64('${tid}'), 8) % 5));
SQL
done

echo "==> ClickHouse: ancient outage clusters (day 87 / 88 / 89) for old-history smoke"
# Forces a visible red cell on the LEFT edge of the day-strip on three
# components — exercises rendering of the very oldest history bucket.
ch -mn <<SQL
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'default',
       now() - toIntervalDay(89) - toIntervalMinute(15 + number * 3),
       'down', 0, 503, 'historical outage (89d ago)'
FROM numbers(6);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${T_WEB}'),'default',
       now() - toIntervalDay(88) - toIntervalMinute(45 + number * 4),
       'down', 0, 504, 'historical outage (88d ago)'
FROM numbers(5);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${T_CDN}'),'default',
       now() - toIntervalDay(87) - toIntervalMinute(120 + number * 5),
       'down', 0, 502, 'historical outage (87d ago)'
FROM numbers(4);
SQL

echo "==> ClickHouse: fix-email 90d history with a 6-day NoData gap"
# Skips day_index 5..10 entirely so those day-strip cells render as
# DayState::NoData (grey, "no data" tooltip) — the only path that produces
# this state. Same 6h shift as the baseline keeps day-0 rows out of the
# last-5-min window.
ch -mn <<SQL
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_EMAIL}'),'default',
       now() - toIntervalHour(6) - toIntervalDay(number) - toIntervalMinute(number % 47),
       'up',
       110 + (number % 25),
       200
FROM numbers(90)
WHERE number < 5 OR number > 10;
SQL

echo "==> ClickHouse: last-5-min divergent writes — drives per-component current_status"
# component_status() looks at the last 5 minutes only. Densify here (30 rows
# at 4s spacing → 2 min span) so the live scheduler's ~5 in-window real
# checks can't tip the classifier away from the intended state. Mix per
# target:
#   fix-api    100% up        → Operational
#   fix-web    100% degraded  → Degraded
#   fix-cdn    ~30% down      → PartialOutage
#   fix-auth   ~70% down      → MajorOutage (>= half)
#   fix-search 100% up        → Operational
# fix-db skipped (Maintenance binding overrides the counter classifier).
# fix-email skipped (no recent rows → counters.total()==0 → Operational
#   fallback, exercising the "no recent data" branch).
ch -mn <<SQL
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'default',
       now() - toIntervalSecond(number*4),
       'up', 90 + (number % 25), 200
FROM numbers(30);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_WEB}'),'default',
       now() - toIntervalSecond(number*4),
       'degraded', 600 + (number * 40), 200
FROM numbers(30);

# Down rows model a TLS-phase failure: DNS + TCP completed (timings present),
# the handshake was rejected (no tls_ms, no response_code) — exercises the
# detail view's expandable partial-timing breakdown on a connect failure.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error,dns_ms,connect_ms)
SELECT toUUID('${ORG}'),toUUID('${T_CDN}'),'default',
       now() - toIntervalSecond(number*4),
       if(number % 10 < 3, 'down', 'up'),
       if(number % 10 < 3, 0, 80 + (number % 30)),
       if(number % 10 < 3, NULL, 200),
       if(number % 10 < 3, 'certificate expired', NULL),
       if(number % 10 < 3, toUInt16(11 + (number % 5)), NULL),
       if(number % 10 < 3, toUInt16(40 + (number % 8)), NULL)
FROM numbers(30);

# Down rows model a TCP-phase failure: DNS resolved (dns_ms present) but the
# connection was refused — connect never completed, so connect_ms stays NULL,
# matching ConnectError::Connect's partial timings.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error,dns_ms)
SELECT toUUID('${ORG}'),toUUID('${T_AUTH}'),'default',
       now() - toIntervalSecond(number*4),
       if(number % 10 < 7, 'down', 'up'),
       if(number % 10 < 7, 0, 95 + (number % 20)),
       if(number % 10 < 7, NULL, 200),
       if(number % 10 < 7, 'connection refused', NULL),
       if(number % 10 < 7, toUInt16(9 + (number % 4)), NULL)
FROM numbers(30);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_SEARCH}'),'default',
       now() - toIntervalSecond(number*4),
       'up', 70 + (number % 20), 200
FROM numbers(30);
SQL

echo "==> ClickHouse: per-region history for fix-api (4 regions) + fix-web (eu-west)"
# Each region reports a visibly different latency shape so the region selector,
# per-region breakdown table, and overlay chart all show distinct lines:
# default fastest, us-east fast, eu-west slower (+history down/error spikes),
# ap-south slowest. fix-web reports from eu-west too.
ch -mn <<SQL
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'eu-west',
       now() - toIntervalHour(6) - toIntervalDay(number) - toIntervalMinute(number % 31),
       'up', 150 + (number % 70), 200
FROM numbers(90);

-- Down + error spikes in HISTORY (not the last-5-min window) so they exercise
-- region-scoped incident/error rendering without flipping the public live
-- status, which sums the last 5 min across regions. Covers down (null
-- response_code + error text) and the error status.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'eu-west',
       now() - toIntervalHour(6) - toIntervalDay(1 + (number * 5) % 40) - toIntervalMinute(number % 60),
       'down', 0, NULL, 'eu-west upstream timeout'
FROM numbers(10);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code,error)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'eu-west',
       now() - toIntervalHour(6) - toIntervalDay(3 + (number * 7) % 40) - toIntervalMinute(number % 60),
       'error', 0, NULL, 'eu-west TLS handshake failed'
FROM numbers(6);

-- Recent window all-up but slower than the default region: keeps the public
-- Operational classification while the region selector still shows distinct
-- per-region latency.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'eu-west',
       now() - toIntervalSecond(number*4),
       'up', 165 + (number % 40), 200
FROM numbers(30);

-- us-east: fast region, clean. Recent window + history.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'us-east',
       now() - toIntervalSecond(number*4),
       'up', 90 + (number % 30), 200
FROM numbers(30);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'us-east',
       now() - toIntervalHour(6) - toIntervalDay(number) - toIntervalMinute(number % 31),
       'up', 95 + (number % 40), 200
FROM numbers(90);

-- ap-south: slowest region, clean. Recent window + history.
INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'ap-south',
       now() - toIntervalSecond(number*4),
       'up', 220 + (number % 60), 200
FROM numbers(30);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_API}'),'ap-south',
       now() - toIntervalHour(6) - toIntervalDay(number) - toIntervalMinute(number % 31),
       'up', 230 + (number % 50), 200
FROM numbers(90);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_WEB}'),'eu-west',
       now() - toIntervalHour(6) - toIntervalDay(number) - toIntervalMinute(number % 31),
       'up', 120 + (number % 30), 200
FROM numbers(90);

INSERT INTO monitor.check_results (org_id,target_id,region,timestamp,status,duration_ms,response_code)
SELECT toUUID('${ORG}'),toUUID('${T_WEB}'),'eu-west',
       now() - toIntervalSecond(number*4),
       'up', 125 + (number % 25), 200
FROM numbers(30);
SQL

echo
echo "=========================================================================="
echo "  Post-seed verification"
echo "=========================================================================="

# ── Postgres row counts ──────────────────────────────────────────────────────
echo
echo "## Postgres counts"
pg -tA <<SQL | column -t -s '|'
SELECT 'targets (fixture)'   AS what, count(*)::text AS n FROM targets
  WHERE org_id='${ORG}' AND tags @> ARRAY['seed-fixtures']
UNION ALL
SELECT 'incidents',            count(*)::text FROM incidents WHERE org_id='${ORG}'
UNION ALL
SELECT '  └ resolved',         count(*)::text FROM incidents WHERE org_id='${ORG}' AND ended_at IS NOT NULL
UNION ALL
SELECT '  └ active',           count(*)::text FROM incidents WHERE org_id='${ORG}' AND ended_at IS NULL
UNION ALL
SELECT 'incident_updates',     count(*)::text FROM incident_updates WHERE org_id='${ORG}'
UNION ALL
SELECT 'maintenance_windows',  count(*)::text FROM maintenance_windows WHERE org_id='${ORG}' AND title LIKE 'Fixture %'
UNION ALL
SELECT '  └ active now',       count(*)::text FROM maintenance_windows
  WHERE org_id='${ORG}' AND title LIKE 'Fixture %' AND starts_at <= now() AND ends_at > now()
UNION ALL
SELECT '  └ component bindings', count(*)::text FROM maintenance_window_components WHERE org_id='${ORG}'
UNION ALL
SELECT 'notification_channels', count(*)::text FROM notification_channels WHERE org_id='${ORG}' AND name LIKE 'Fixture %'
UNION ALL
SELECT 'targets w/ alerts',    count(*)::text FROM targets
  WHERE org_id='${ORG}' AND tags @> ARRAY['seed-fixtures'] AND jsonb_array_length(alerts) > 0;
SQL

# ── Regions, assignments, agents (multi-region smoke surface) ─────────────────
echo
echo "## Regions & agents"
pg -tA <<SQL | column -t -s '|'
SELECT 'regions'              AS what, count(*)::text AS n FROM regions
UNION ALL
SELECT 'agents',              count(*)::text FROM agents
UNION ALL
SELECT '  └ stale (>90s)',    count(*)::text FROM agents
  WHERE enabled AND coalesce(last_seen_at, created_at) < now() - interval '90 seconds'
UNION ALL
SELECT 'multi-region targets', count(*)::text FROM (
  SELECT tr.target_id FROM target_regions tr JOIN targets t ON t.id = tr.target_id
   WHERE t.org_id='${ORG}' AND t.tags @> ARRAY['seed-fixtures']
   GROUP BY tr.target_id HAVING count(*) > 1) m
UNION ALL
SELECT 'multi-region-policy targets', count(*)::text FROM targets
  WHERE org_id='${ORG}' AND tags @> ARRAY['seed-fixtures']
    AND region_policy <> '"majority"'::jsonb;
SQL

# ── ClickHouse last-5-min counters + expected/actual state matrix ────────────
echo
echo "## ClickHouse last-5-min counters per public component"
ch_out=$(ch -q "
SELECT spc.public_name, spc.sort_order, spc.public_group,
       coalesce(c.up_, 0)   AS up_,
       coalesce(c.down_, 0) AS down_,
       coalesce(c.deg_, 0)  AS deg_,
       coalesce(c.err_, 0)  AS err_
FROM postgresql('${PG_CONTAINER}:5432','monitor','status_page_components','monitor','monitor') spc
LEFT JOIN (
  SELECT target_id,
         countIf(status='up')       AS up_,
         countIf(status='down')     AS down_,
         countIf(status='degraded') AS deg_,
         countIf(status='error')    AS err_
  FROM monitor.check_results
  WHERE timestamp >= now() - INTERVAL 5 MINUTE
  GROUP BY target_id
) c ON c.target_id = spc.target_id
WHERE spc.org_id=toUUID('${ORG}')
ORDER BY ifNull(spc.public_group, 'zzz'), spc.sort_order
FORMAT TabSeparated")

# Expected state matrix — keep in lockstep with header comment above.
declare -A EXPECT=(
  [API]=Operational
  [Website]=Degraded
  [CDN]=PartialOutage
  [Database]=Maintenance
  ['Auth Service']=MajorOutage
  ['Email Delivery']=Operational
  [Search]=Operational
  ['Beta Sandbox']=Operational
)

classify() {
  local up=$1 down=$2 deg=$3 err=$4 name=$5
  # Maintenance binding wins regardless of counters.
  if [[ "$name" == "Database" ]]; then echo Maintenance; return; fi
  local total=$((up + down + deg + err))
  local hard=$((down + err))
  if (( total == 0 )); then echo Operational; return; fi
  if (( hard == 0 )); then
    if (( deg > 0 )); then echo Degraded; else echo Operational; fi
    return
  fi
  if (( hard * 2 >= total )); then echo MajorOutage; else echo PartialOutage; fi
}

# Hard failures (non-zero exit); NOTES are non-fatal observations.
FAILED=()
NOTES=()
printf '  %-18s %-14s %5s %5s %5s %5s   %s\n' name expected up down deg err actual
while IFS=$'\t' read -r name _ord _group up down deg err; do
  [[ -z "$name" ]] && continue
  actual=$(classify "$up" "$down" "$deg" "$err" "$name")
  expected="${EXPECT[$name]:-?}"
  mark="OK"
  if [[ "$actual" != "$expected" ]]; then
    mark="MISMATCH"
    FAILED+=("clickhouse component '${name}': expected ${expected}, classified ${actual} (up=${up} down=${down} deg=${deg} err=${err}) — check last-5-min seed writes")
  fi
  printf '  %-18s %-14s %5s %5s %5s %5s   %s [%s]\n' \
    "$name" "$expected" "$up" "$down" "$deg" "$err" "$actual" "$mark"
done <<< "$ch_out"

# ── HTTP smoke ───────────────────────────────────────────────────────────────
echo
echo "## Public page render"
http_url="http://${SLUG}.${BASE_DOMAIN}:8080/"
echo "  host ${SLUG}.${BASE_DOMAIN} resolves against status_pages.slug='${SLUG}' (subdomain routing must be ON)"
# Cache TTL is ~10s; give the aggregator one beat so the freshly-seeded rows
# replace any prior cached page.
sleep 12
read -r status_code time_total < <(curl -s -o /tmp/seed-fixtures-pub.html \
  -w '%{http_code} %{time_total}\n' "$http_url" 2>/dev/null || echo '000 0')
echo "  GET $http_url → HTTP $status_code (${time_total}s)"
if [[ "$status_code" == "200" ]]; then
  states=$(grep -oE '>Operational<|>Degraded<|>Partial outage<|>Major outage<|>Maintenance<' \
           /tmp/seed-fixtures-pub.html | sort | uniq -c | awk '{print $2" "$3" ×"$1}')
  archive=$(grep -c 'Older incidents' /tmp/seed-fixtures-pub.html || true)
  body_bytes=$(wc -c < /tmp/seed-fixtures-pub.html | tr -d ' ')
  echo "  body size: ${body_bytes} bytes"
  echo "  rendered badges:"
  echo "$states" | sed 's/^/    /'
  echo "  'Older incidents →' archive link present: $([[ $archive -gt 0 ]] && echo yes || echo no)"
else
  case "$status_code" in
    000) hint="connection refused — is the app running on :8080?" ;;
    303) hint="redirected (login) — subdomain routing OFF; run with UPTIMEPAGE_TENANCY__SUBDOMAIN_PUBLIC_ROUTES=true and UPTIMEPAGE_PUBLIC_STATUS__BASE_DOMAIN=${BASE_DOMAIN}" ;;
    404) hint="no enabled status page with slug '${SLUG}' — the org's public page slug must equal the subdomain label" ;;
    *)   hint="unexpected status — check application logs" ;;
  esac
  echo "  WARN: page did not return 200 — $hint"
  FAILED+=("public page render: HTTP ${status_code} — ${hint}")
fi

# ── Operator Monitors page render (group + owner + bulk markup) ─────────────
echo
echo "## Operator Monitors page render"
monitors_url="http://app.${BASE_DOMAIN}:8080/targets"
cookies_file="${HOME}/.uptimepage-dev-cookies"
curl_auth=()
if [[ -f "$cookies_file" ]]; then
  curl_auth=(-b "$cookies_file")
fi
read -r monitors_status monitors_time < <(curl -s -o /tmp/seed-fixtures-monitors.html \
  -w '%{http_code} %{time_total}\n' "${curl_auth[@]}" "$monitors_url" 2>/dev/null || echo '000 0')
echo "  GET $monitors_url → HTTP $monitors_status (${monitors_time}s)"
if [[ "$monitors_status" == "200" ]]; then
  group_count=$(grep -c 'class="monitors-group"' /tmp/seed-fixtures-monitors.html || true)
  row_count=$(grep -c 'data-row-id=' /tmp/seed-fixtures-monitors.html || true)
  bulk_present=$(grep -c 'id="monitors-bulk"' /tmp/seed-fixtures-monitors.html || true)
  avatar_count=$(grep -c 'class="monitors-avatar"' /tmp/seed-fixtures-monitors.html || true)
  ungrouped_present=$(grep -c 'data-group-name="Ungrouped"' /tmp/seed-fixtures-monitors.html || true)
  echo "    group cards rendered  : $group_count   (expected ≥ 5: API & Web, CDN, Infrastructure, Notifications, Beta, Integrations, Ungrouped)"
  echo "    monitor rows rendered : $row_count    (expected 14)"
  echo "    bulk-bar present      : $([[ $bulk_present -gt 0 ]] && echo yes || echo no)"
  echo "    owner avatars         : $avatar_count   (≥ 1 unless org has no members)"
  echo "    Ungrouped group       : $([[ $ungrouped_present -gt 0 ]] && echo yes || echo no)   (fix-search → NULL group_name)"
  if (( group_count < 5 || row_count < 14 || bulk_present < 1 )); then
    echo "    WARN: Monitors page render below expected baseline — check template + view wiring"
    FAILED+=("operator monitors render: groups=${group_count} rows=${row_count} bulk=${bulk_present} (want ≥5 / 14 / ≥1)")
  fi
else
  echo "  (page requires auth — re-run 'just dev-login' so cookies land in $cookies_file)"
  NOTES+=("operator monitors check skipped: HTTP ${monitors_status} (no dev-login cookie)")
fi

# ── Adversarial-title escape verification ────────────────────────────────────
echo
echo "## Adversarial-title escape"
if grep -q 'Adversarial title &#60;/script&#62;' /tmp/seed-fixtures-pub.html 2>/dev/null; then
  echo "  HTML body : entity-encoded ✓"
else
  echo "  HTML body : NOT FOUND or not encoded — verify XSS escape path"
  FAILED+=("adversarial-title HTML escape: encoded marker absent (public page may have failed to render)")
fi
if grep -q 'Adversarial title \\u003c/script\\u003e' /tmp/seed-fixtures-pub.html 2>/dev/null; then
  echo "  JSON blob : \\u-escaped ✓"
else
  echo "  JSON blob : NOT FOUND — day-popover JSON may not be rendering"
  NOTES+=("adversarial-title JSON escape: \\u-escaped marker absent (day-popover JSON or page render)")
fi

# ── Day-strip pattern (1 char per day, oldest→newest) ────────────────────────
echo
echo "## Day-strip pattern (left=89d ago, right=today)"
python3 - /tmp/seed-fixtures-pub.html <<'PY' || true
import re, sys
html = open(sys.argv[1]).read()
sym = {'op':'.','deg':'D','part':'p','maj':'M','mnt':'m','none':'_'}
# Match each day-cell, capture the state class.
cells = re.findall(r'day-cell day-cell--([a-z-]+)', html)
# Match component names from the <h3> headers preceding each strip.
names = re.findall(r'class="component-name[^>]*>([^<]+)<', html)
n = 90
strips = [cells[i*n:(i+1)*n] for i in range(len(cells)//n)]
for i, s in enumerate(strips):
    label = (names[i] if i < len(names) else f'comp{i}')[:14]
    print(f'  {label:14s} {"".join(sym.get(c,"?") for c in s)}')
PY

echo
echo "=========================================================================="
if (( ${#NOTES[@]} > 0 )); then
  echo "  NOTES (non-fatal):"
  for n in "${NOTES[@]}"; do echo "    • $n"; done
fi
if (( ${#FAILED[@]} == 0 )); then
  echo "  RESULT: all checks passed ✓"
else
  echo "  RESULT: ${#FAILED[@]} check(s) failed:"
  for f in "${FAILED[@]}"; do echo "    ✗ $f"; done
fi
echo "=========================================================================="
echo
echo "Seeded org '${SLUG}' (id ${ORG})."
echo "  monitors  : 14 (7 public/visible HTTP + 1 disabled public + 2 internal HTTP + 4 non-HTTP)"
echo "  groups    : API & Web · CDN · Infrastructure · Notifications · Beta · Integrations · Ungrouped"
echo "  owners    : $([[ -n "$OWNER_USER_ID" ]] && echo "8 monitors bound to ${OWNER_USER_ID:0:8}…" || echo 'no member found — every monitor unowned')"
echo "  incidents : 158 (150 resolved across 87d + 4 active (one per frozen public monitor)
              + 1 adversarial-title + 2 internal-only on non-public monitors + 1 manually declared)
  ops state : active public incidents split triggered/acknowledged; internal + manual
              incidents carry activity timelines for the operator console"
echo "  channels  : 9 (slack + webhook + whatsapp + discord + msteams + google_chat enabled; email enabled but unverified; telegram disabled; telegram_app disabled with unlink note)"
echo "  managed   : fix-payment/fix-admin + Fixture Webhook → terraform chip; fix-tcp + Fixture Slack → api chip"
echo "  alerts    : bound on fix-api / fix-db / fix-auth"
echo "  maintenance: 4 windows (1 active bound to fix-db) → drives Maintenance state"
echo "  history   : 6-day NoData gap on fix-email; per-target last-5-min divergence"
echo
echo "Public status page : http://${SLUG}.${BASE_DOMAIN}:8080/"
echo "Operator dashboard : http://app.${BASE_DOMAIN}:8080/"
echo "Operator monitors  : http://app.${BASE_DOMAIN}:8080/targets"
echo "Operator incidents : http://app.${BASE_DOMAIN}:8080/incidents"
echo "(public page cache TTL ~10s; wait a moment before first load)"

exit $(( ${#FAILED[@]} > 0 ))
