#!/usr/bin/env bash
# Fails if any tenant-table SQL is missing `org_id` in its WHERE clause, or
# if an AdminRepo construction uses a dynamic reason. Run locally before a
# PR; CI runs it on every push.
#
# Allow-listed paths:
#   - src/storage/admin.rs    — AdminRepo: cross-tenant by design.
#   - src/storage/orgs.rs     — manages the `organizations` table itself.
#   - migrations/**           — DDL, not runtime SQL.

set -euo pipefail

cd "$(dirname "$0")/.."

ast-grep scan \
  src/ \
  --globs '!src/storage/admin.rs' \
  --globs '!src/storage/orgs.rs'

scripts/check_clickhouse_org_scope.sh
