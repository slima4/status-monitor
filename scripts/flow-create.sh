#!/usr/bin/env bash
# Instantly test, then create, a sample browser-flow login monitor against the
# local dev stack. Pairs with `just flow-dev` (running) + `just dev-login`
# (session seeded). No UI exists for flow yet, so this is the way to exercise it.
#
# Overrides via env: BASE_URL, DEV_COOKIE, NAME.
set -euo pipefail

BASE_URL="${BASE_URL:-http://app.lvh.me:8080}"
COOKIE="${DEV_COOKIE:-_sm_session=devsession-localtest-0000000000}"
NAME="${NAME:-flow-login-demo}"
PG_CONTAINER="${PG_CONTAINER:-uptimepage-postgres-1}"

# Flow is plan-gated (max_flow_checks) and 0 by default. Raise it on the dev plans
# now that the app has booted and migrated the column in. Idempotent.
echo "== Enable flow on the dev plan =="
if docker exec -i "${PG_CONTAINER}" psql -U monitor -d monitor \
    -c "UPDATE plans SET max_flow_checks = 5" >/dev/null 2>&1; then
  echo "  max_flow_checks = 5"
else
  echo "  skipped — is the stack up and 'just flow-dev' running (so migrations ran)?"
fi
echo

# A public login that always passes: fills the form, submits, asserts the
# post-login URL and greeting. Swap in your own site + selectors to try it.
read -r -d '' SPEC <<'JSON' || true
{
  "type": "flow",
  "start_url": "https://the-internet.herokuapp.com/login",
  "steps": [
    {"op": "fill", "selector": "#username", "value": "tomsmith"},
    {"op": "fill", "selector": "#password", "value": "SuperSecretPassword!"},
    {"op": "click", "selector": "button[type=\"submit\"]"},
    {"op": "assert_url", "contains": "/secure"},
    {"op": "assert_text", "contains": "secure area"}
  ],
  "timeout": 30000,
  "step_timeout": 10000,
  "verify_tls": true
}
JSON

pp() { if command -v jq >/dev/null 2>&1; then jq "$@"; else cat; fi; }

# State-changing owner API: the session cookie authenticates, and the custom
# header satisfies the same-origin CSRF guard.
HDR=(-H "Cookie: ${COOKIE}" -H "X-Requested-With: uptimepage" -H "Content-Type: application/json")

echo "== Test run (instant, not persisted — spawns a real browser, ~5-10s) =="
curl -sS -X POST "${BASE_URL}/api/v1/targets/test" "${HDR[@]}" \
  -d "{\"check\": ${SPEC}}" \
  | pp '{region, matched: .matched_expectations, status: .result.status, error: .result.error}'

echo
echo "== Create persisted monitor (300s interval) =="
curl -sS -X POST "${BASE_URL}/api/v1/targets" "${HDR[@]}" \
  -d "{\"name\": \"${NAME}\", \"interval\": 300, \"check\": ${SPEC}}" \
  | pp '{id, name, enabled, kind: .check.type}'

echo
echo "The scheduler re-runs it every 300s; force one now with:"
echo "  curl -sS -X POST ${BASE_URL}/api/v1/targets/<id>/check-now \\"
echo "    -H 'Cookie: ${COOKIE}' -H 'X-Requested-With: uptimepage' | jq ."
