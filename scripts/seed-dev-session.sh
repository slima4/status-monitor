#!/usr/bin/env bash
# Seed an authenticated operator session for local SaaS-mode testing.
#
# Self-host (tenancy off) bypasses the session cookie, so the owner-gated
# operator APIs (status-page settings, members, …) can't be exercised. The
# dev stack runs with tenancy ON (see compose.dev.yml); this script mints the
# user + org + owner membership + a session row so a browser cookie — or a
# curl `Cookie:` header — authenticates as the org owner. No GitHub OAuth
# needed.
#
# Idempotent: re-running upserts the same rows and refreshes the session
# expiry. Requires the dev stack already up (`just up-app`).
#
# Env overrides:
#   EMAIL         operator email          (default: dev@local.test)
#   SLUG          org slug                (default: devorg)
#   ORG_NAME      org display name        (default: Dev Org)
#   TOKEN         session cookie value    (default: devsession-localtest-0000000000)
#   PG_CONTAINER  postgres container name (default: status-monitor-postgres-1)
set -euo pipefail

EMAIL="${EMAIL:-dev@local.test}"
SLUG="${SLUG:-devorg}"
ORG_NAME="${ORG_NAME:-Dev Org}"
TOKEN="${TOKEN:-devsession-localtest-0000000000}"
PG_CONTAINER="${PG_CONTAINER:-status-monitor-postgres-1}"
BASE_URL="${BASE_URL:-http://localhost:8080}"

pg() { docker exec -i "$PG_CONTAINER" psql -U monitor -d monitor -v ON_ERROR_STOP=1 "$@"; }

pg <<SQL
WITH u AS (
  INSERT INTO users (email) VALUES ('${EMAIL}')
  ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email
  RETURNING id
), o AS (
  INSERT INTO organizations (slug, name) VALUES ('${SLUG}', '${ORG_NAME}')
  ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name
  RETURNING id
), m AS (
  INSERT INTO memberships (user_id, org_id, role)
  SELECT u.id, o.id, 'owner' FROM u, o
  ON CONFLICT (user_id, org_id) DO UPDATE SET role = 'owner'
  RETURNING 1
)
INSERT INTO sessions (id, user_id, active_org_id, expires_at)
SELECT '${TOKEN}', u.id, o.id, now() + interval '30 days'
FROM u, o
ON CONFLICT (id) DO UPDATE
  SET expires_at = EXCLUDED.expires_at, last_used_at = now();
SQL

cat <<EOF

Seeded owner session.
  email : ${EMAIL}
  org   : ${SLUG} (${ORG_NAME}), role=owner
  token : ${TOKEN}

Browser — paste in the devtools Console at ${BASE_URL}, then reload:
  document.cookie = "_sm_session=${TOKEN}; path=/";

curl — pass the cookie directly:
  curl -i -X PATCH ${BASE_URL}/api/v1/orgs/\$(docker exec -i ${PG_CONTAINER} \\
      psql -U monitor -d monitor -tAc "select id from organizations where slug='${SLUG}'")/status-page \\
    -H 'content-type: application/json' \\
    -H 'X-Requested-With: status-monitor' \\
    -H 'Cookie: _sm_session=${TOKEN}' \\
    -d '{"public_status_enabled":true,"public_brand_color":"#3b82f6"}'

Then open ${BASE_URL}/settings/status-page
EOF
