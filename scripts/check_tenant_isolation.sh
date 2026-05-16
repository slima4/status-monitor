#!/usr/bin/env bash
# Fails if any tenant-table SQL is missing `org_id` in its WHERE clause, or
# if an AdminRepo construction uses a dynamic reason. Run locally before a
# PR; CI runs it on every push.
#
# Allow-listed paths:
#   - src/storage/admin.rs              — AdminRepo: cross-tenant by design.
#   - src/storage/orgs.rs               — manages the `organizations` table itself.
#   - src/jobs/purge_deleted.rs         — drains soft-deleted orgs + users across tenants.
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
  --globs '!src/quotas/service.rs'

scripts/check_clickhouse_org_scope.sh
