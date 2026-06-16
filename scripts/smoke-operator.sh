#!/usr/bin/env bash
# One-click smoke test for the operator (instance-admin) surface: regions +
# agents. Self-contained and self-cleaning — it creates a throwaway region +
# agent, asserts every status code, then removes them (no leftover fixtures).
#
# Requires the app running with UPTIMEPAGE_OPERATOR__ADMIN_TOKEN set; pass the
# same value in:
#
#   OPERATOR_TOKEN=<that-secret> ./scripts/smoke-operator.sh
#
# Optional: BASE=http://app.lvh.me:8080 (default).
set -uo pipefail

BASE="${BASE:-http://app.lvh.me:8080}"
TOKEN="${OPERATOR_TOKEN:-}"
if [[ -z $TOKEN ]]; then
  echo "error: set OPERATOR_TOKEN to the app's UPTIMEPAGE_OPERATOR__ADMIN_TOKEN" >&2
  exit 2
fi
AUTH=(-H "Authorization: Bearer ${TOKEN}")
PASS=0
FAIL=0
AGENT_ID=""
CODE=""
BODY=""

http() { # METHOD PATH [JSON] → authed; sets CODE + BODY
  local m=$1 p=$2 d=${3:-} resp
  if [[ -n $d ]]; then
    resp=$(curl -sS -X "$m" "${AUTH[@]}" -H 'Content-Type: application/json' \
                -d "$d" -w $'\n%{http_code}' "$BASE$p")
  else
    resp=$(curl -sS -X "$m" "${AUTH[@]}" -w $'\n%{http_code}' "$BASE$p")
  fi
  CODE=${resp##*$'\n'}
  BODY=${resp%$'\n'*}
}

expect() { # LABEL WANT_CODE
  if [[ ${CODE:-} == "$2" ]]; then
    printf '  \033[32mPASS\033[0m  %-34s %s\n' "$1" "$CODE"
    PASS=$((PASS + 1))
  else
    printf '  \033[31mFAIL\033[0m  %-34s got %s, want %s\n        %s\n' \
      "$1" "${CODE:-?}" "$2" "${BODY:-}"
    FAIL=$((FAIL + 1))
  fi
}

note_pass() { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS + 1)); }
note_fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL + 1)); }

cleanup() {
  [[ -n $AGENT_ID ]] &&
    curl -sS -o /dev/null -X DELETE "${AUTH[@]}" "$BASE/operator/agents/$AGENT_ID" 2>/dev/null
  curl -sS -o /dev/null -X DELETE "${AUTH[@]}" "$BASE/operator/regions/smoke-test" 2>/dev/null
  true
}
trap cleanup EXIT

echo "operator smoke @ $BASE"

http GET /operator/regions
expect "list regions" 200

http GET /operator/agents
expect "list agents" 200

# Secret IS configured (we hold it), so a missing token is 401 — not the 404
# that only the disabled (unset-secret) surface returns.
no_tok=$(curl -sS -w $'\n%{http_code}' "$BASE/operator/regions")
CODE=${no_tok##*$'\n'}
BODY=${no_tok%$'\n'*}
expect "no-token rejected" 401

http POST /operator/regions '{"id":"smoke-test","name":"Smoke","city":"nowhere"}'
expect "create region" 201

http POST /operator/regions '{"id":"smoke-test","name":"Smoke"}'
expect "duplicate region" 409

http POST /operator/regions '{"id":"Bad ID!","name":"x"}'
expect "reject bad slug" 400

http POST /operator/agents '{"region":"smoke-test","name":"Smoke probe"}'
expect "mint agent" 201
AGENT_ID=$(printf '%s' "$BODY" | sed -n 's/.*"id":"\([0-9a-fA-F-]\{36\}\)".*/\1/p')
if printf '%s' "$BODY" | grep -q '"token":"sm_agent_'; then
  note_pass "agent token returned once (sm_agent_…)"
else
  note_fail "agent token missing from create response"
fi

http DELETE /operator/regions/eu-helsinki
expect "delete in-use region (eu-helsinki)" 409

http DELETE /operator/regions/smoke-test
expect "delete region holding agent" 409

if [[ -n $AGENT_ID ]]; then
  http DELETE "/operator/agents/$AGENT_ID"
  expect "delete agent" 204
  AGENT_ID=""
else
  note_fail "could not parse minted agent id — skipping agent delete"
fi

http DELETE /operator/regions/smoke-test
expect "delete now-empty region" 204

echo
echo "  $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
