#!/usr/bin/env bash
# Fails if any tenant-table SQL is missing `org_id` in its WHERE clause, or
# if an AdminRepo construction uses a dynamic reason. Run locally before a
# PR; CI runs it on every push.
#
# Allow-listed paths:
#   - src/storage/admin.rs              — AdminRepo: cross-tenant by design.
#   - src/storage/orgs.rs               — manages the `organizations` table itself.
#   - src/jobs/purge_deleted.rs         — drains soft-deleted orgs + users across tenants.
#   - src/jobs/retention.rs             — daily cross-tenant retention sweep.
#   - src/quotas/service.rs             — resolves one org's plan via the
#                                         organizations PK (`o.id = $1`); the
#                                         scanner greps for literal `org_id`
#                                         and can't see PK scoping. Returns
#                                         plan config, never tenant rows.
#   - migrations/**                     — DDL, not runtime SQL.

set -euo pipefail

cd "$(dirname "$0")/.."

ast-grep scan \
  src/ \
  --globs '!src/storage/admin.rs' \
  --globs '!src/storage/orgs.rs' \
  --globs '!src/jobs/purge_deleted.rs' \
  --globs '!src/jobs/retention.rs' \
  --globs '!src/quotas/service.rs'

scripts/check_clickhouse_org_scope.sh

# The operator target/results repos must stay org-less at construction: the
# `org` is a per-call argument resolved from the request's `CurrentOrg`, so
# "forgot to scope" is a compile error rather than a cross-tenant leak.
# Re-adding an ambient org to the constructor (a third arg to
# `PostgresTargetStore::from_pool` / a second to
# `ClickhouseResultsStore::from_client`) silently re-opens the IDOR; the type
# system can't catch that, so grep does. `ClickhouseResultSink` (write side)
# legitimately keeps its org and is intentionally not matched here.
ambient_org="$(rg --pcre2 --multiline --line-number \
  -e 'PostgresTargetStore::from_pool\(\s*[^,]+,\s*[^,)]+,\s*[^)]+\)' \
  -e 'ClickhouseResultsStore::from_client\(\s*[^,)]+,\s*[^)]+\)' \
  src/ tests/ benches/ || true)"

if [ -n "$ambient_org" ]; then
  echo "tenant-isolation check failed: target/results store constructed with an ambient org." >&2
  echo "Drop the org arg; pass it per call from CurrentOrg instead." >&2
  echo >&2
  echo "$ambient_org" >&2
  exit 1
fi
