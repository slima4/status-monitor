#!/usr/bin/env bash
# PG-side tenant isolation: every SQL statement-literal touching a
# tenant-scoped table must include `org_id` in its WHERE clause.
# Implemented as the ast-grep rule scripts/sg-rules/no_raw_tenant_sql.yml.
#
# File allow-list (cross-tenant by design):
#   - src/storage/admin.rs              AdminRepo
#   - src/storage/orgs.rs               manages the `organizations` table itself
#   - src/jobs/purge_deleted.rs         soft-deleted org/user drain
#   - src/jobs/retention.rs             daily cross-tenant retention sweep
#   - src/quotas/service.rs             plan resolution via organizations PK
#
# Per-statement escape hatch: inline `/* SAFE: <reason> */` SQL block
# comment opts a single statement out. Use sparingly. Block-comment
# syntax is mandatory — see check_safe_comment_syntax.sh.

set -euo pipefail

cd "$(dirname "$0")/.."

ast-grep scan \
  --rule scripts/sg-rules/no_raw_tenant_sql.yml \
  src/ \
  --globs '!src/storage/admin.rs' \
  --globs '!src/storage/orgs.rs' \
  --globs '!src/jobs/purge_deleted.rs' \
  --globs '!src/jobs/retention.rs' \
  --globs '!src/quotas/service.rs'
