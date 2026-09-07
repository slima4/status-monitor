#!/usr/bin/env bash
# Puts the dev account into the state the plan-holds UI is for: more monitors
# and status pages than the plan covers, with the excess held.
#
# It does NOT move the account between plans. An org's plan is cached for
# quotas.plan_cache_ttl_secs (300s), so a plan changed underneath a running app
# is invisible for minutes and every attempt to build the fixture by downgrading
# races that cache. The row count is the one lever that takes effect at once.
#
# So: everything the plan allows is created through the API, the real way. The
# overflow — the part that makes the account over-cap — is inserted directly,
# because the API refuses by design to create an over-cap state. That is honest
# to what a hold is: not something a customer can ask for, but what is left when
# a plan shrinks under rows that already exist.
#
# Needs the dev stack up and `scripts/seed-dev-session.sh` already run.
#
#   bash scripts/seed-dev-holds.sh                    # cap + 4 monitors, cap + 1 pages
#   OVER=12 bash scripts/seed-dev-holds.sh            # more overflow
#   MONITORS=30 bash scripts/seed-dev-holds.sh        # an exact number instead
#   RECONCILE_ONLY=1 bash scripts/seed-dev-holds.sh   # just re-run the reconcile
#   RESET=1 bash scripts/seed-dev-holds.sh            # delete what this made, then reseed
set -euo pipefail

TOKEN="${TOKEN:-devsession-localtest-0000000000}"
SLUG="${SLUG:-devorg}"
PG_CONTAINER="${PG_CONTAINER:-uptimepage-postgres-1}"
BASE_URL="${BASE_URL:-http://app.lvh.me:8080}"
# Totals wanted. Left unset they are read from the plan as cap + OVER, because
# a hard-coded number is only over the cap of whichever plan it was written
# against — 24 monitors overflows free and sits well inside founding, seeding
# nothing to look at.
MONITORS="${MONITORS:-}"
PAGES="${PAGES:-}"
OVER="${OVER:-4}"
PREFIX="${PREFIX:-seed-}"

pg() { docker exec -i "$PG_CONTAINER" psql -U monitor -d monitor -v ON_ERROR_STOP=1 -tAc "$1"; }
# Returns "<code> <body>". The body matters: every failure here is the API
# telling you exactly what it disliked, and swallowing it turns a one-line fix
# into guesswork.
api() {
  local method="$1" path="$2" body="${3:-}"
  local args=(-sS -m 15 -w '\n%{http_code}' -X "$method" "${BASE_URL}${path}"
              -H 'content-type: application/json'
              -H 'X-Requested-With: uptimepage'
              -H "Cookie: _sm_session=${TOKEN}")
  [ -n "$body" ] && args+=(-d "$body")
  local out code
  out=$(curl "${args[@]}" 2>/dev/null) || { echo "000 "; return; }
  code=$(printf '%s' "$out" | tail -n1)
  printf '%s %s' "$code" "$(printf '%s' "$out" | sed '$d' | tr -d '\n' | cut -c1-300)"
}

ORG=$(pg "SELECT id FROM organizations WHERE slug='${SLUG}' AND deleted_at IS NULL")
if [ -z "$ORG" ]; then
  echo "no org '${SLUG}' — run scripts/seed-dev-session.sh first" >&2
  exit 1
fi
ACCOUNT=$(pg "SELECT account_id FROM organizations WHERE id='${ORG}'")

reconcile() {
  # An empty body names no pick, leaving whatever the customer chose alone and
  # reconciling as a side effect of saving. Sending an empty list per resource
  # would instead clear the pick, which is not what a re-run should do.
  local code
  api PUT /api/v1/account/holds '{}'
}

report() {
  local held_t held_p all_t all_p plan
  held_t=$(pg "SELECT count(*) FROM targets WHERE org_id='${ORG}' AND plan_hold_at IS NOT NULL")
  held_p=$(pg "SELECT count(*) FROM status_pages WHERE org_id='${ORG}' AND plan_hold_at IS NOT NULL")
  all_t=$(pg "SELECT count(*) FROM targets WHERE org_id='${ORG}'")
  all_p=$(pg "SELECT count(*) FROM status_pages WHERE org_id='${ORG}'")
  plan=$(pg "SELECT plan_id FROM accounts WHERE id='${ACCOUNT}'")

  cat <<EOF

Account is on ${plan}.
  monitors     : ${all_t} total, ${held_t} held
  status pages : ${all_p} total, ${held_p} held
EOF

  if [ "$held_t" = "0" ] && [ "$held_p" = "0" ]; then
    cat <<EOF

Nothing is held. The account is not over any cap, so there is nothing to hold.
Ask for a bigger overflow and run it again:

  OVER=12 bash scripts/seed-dev-holds.sh

EOF
    return
  fi

  cat <<EOF

Look at:
  ${BASE_URL}/settings/usage    the holds panel and the keep picker
  ${BASE_URL}/targets           held monitors wear a 'held' chip, still listed
  ${BASE_URL}/settings/pages    a held page reads 'held' instead of published

Release them again — delete the overflow, then reconcile:
  docker exec -i ${PG_CONTAINER} psql -U monitor -d monitor \\
    -c "DELETE FROM targets WHERE org_id='${ORG}' AND name LIKE '${PREFIX}over-%'"
  bash scripts/seed-dev-holds.sh   # RECONCILE_ONLY=1 skips rebuilding

Start over:
  RESET=1 bash scripts/seed-dev-holds.sh
EOF
}

if [ -n "${RECONCILE_ONLY:-}" ]; then
  echo "reconciling…"
  res=$(reconcile)
  [ "${res%% *}" = "200" ] || echo "  reconcile: ${res}" >&2
  report
  exit 0
fi

if [ -n "${RESET:-}" ]; then
  echo "removing anything this script seeded before…"
  pg "DELETE FROM targets WHERE org_id='${ORG}' AND name LIKE '${PREFIX}%'" >/dev/null
  pg "DELETE FROM status_pages WHERE org_id='${ORG}' AND name LIKE '${PREFIX}%'" >/dev/null
fi

PLAN_ID=$(pg "SELECT plan_id FROM accounts WHERE id='${ACCOUNT}'")
CAP_T=$(pg "SELECT max_targets FROM plans WHERE id='${PLAN_ID}'")
CAP_P=$(pg "SELECT max_status_pages FROM plans WHERE id='${PLAN_ID}'")
MONITORS="${MONITORS:-$(( CAP_T + OVER ))}"
PAGES="${PAGES:-$(( CAP_P + 1 ))}"
echo "account is on ${PLAN_ID}: ${CAP_T} monitors, ${CAP_P} status pages"
echo "seeding ${MONITORS} monitors and ${PAGES} status pages, so ${OVER} and 1 end up held"

have_t=$(pg "SELECT count(*) FROM targets WHERE org_id='${ORG}'")
have_p=$(pg "SELECT count(*) FROM status_pages WHERE org_id='${ORG}'")

# Up to the cap, through the front door.
via_api=$(( CAP_T - have_t ))
[ "$via_api" -lt 0 ] && via_api=0
[ "$via_api" -gt "$MONITORS" ] && via_api="$MONITORS"
echo "creating ${via_api} monitors through the API…"
created=0
for i in $(seq 1 "$via_api"); do
  res=$(api POST /api/v1/targets "$(cat <<JSON
{"name": "${PREFIX}monitor-${i}",
 "check": {"type": "http", "url": "https://example.com/${i}", "method": "GET",
           "timeout": 5000, "follow_redirects": false, "max_redirects": 0,
           "expected_status": {"kind": "exact", "value": 200},
           "headers": {}, "verify_tls": true},
 "interval": 300}
JSON
)")
  case "${res%% *}" in
    201) created=$((created + 1)) ;;
    409) : ;;
    000) echo "  cannot reach ${BASE_URL} — is the app running?" >&2; exit 1 ;;
    *) if [ "${res}" != "${last_err:-}" ]; then echo "  monitor: ${res}"; last_err="${res}"; fi ;;
  esac
done
echo "  ${created} created"

# The overflow. Inserted rather than requested, because a create that would put
# the account over its cap is refused — which is the whole point of the cap.
have_t=$(pg "SELECT count(*) FROM targets WHERE org_id='${ORG}'")
over_t=$(( MONITORS - have_t ))
if [ "$over_t" -gt 0 ]; then
  echo "inserting ${over_t} monitors past the cap, the way a shrunken plan leaves them…"
  pg "INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled)
      SELECT '${ORG}', '${PREFIX}over-' || g,
             jsonb_build_object(
               'type', 'http', 'url', 'https://example.com/over-' || g,
               'method', 'GET', 'timeout', 5000, 'follow_redirects', false,
               'max_redirects', 0, 'headers', '{}'::jsonb, 'verify_tls', true,
               'expected_status', jsonb_build_object('kind','exact','value',200)),
             300, true
      FROM generate_series(1, ${over_t}) g" >/dev/null
fi

over_p=$(( PAGES - have_p ))
if [ "$over_p" -gt 0 ]; then
  echo "adding ${over_p} status page(s) past the cap…"
  pg "INSERT INTO status_pages (org_id, slug, name, enabled)
      SELECT '${ORG}', '${PREFIX}page-' || g, '${PREFIX}page-' || g, true
      FROM generate_series(1, ${over_p}) g
      ON CONFLICT (slug) DO NOTHING" >/dev/null
fi

echo "reconciling…"
res=$(reconcile)
[ "${res%% *}" = "200" ] || echo "  reconcile: ${res}" >&2
report
