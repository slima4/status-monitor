#!/usr/bin/env bash
# Tenant-isolation gate. Composes four per-concern checks; each can be
# run on its own when debugging a single failure.
#
#   check_pg_org_scope.sh                 PG: tenant SQL must bind org_id
#   check_clickhouse_org_scope.sh         CH: check_results target_id
#                                         filter must also bind org_id
#   check_safe_comment_syntax.sh          `-- SAFE:` line-comment footgun
#   check_store_construction_org_less.sh  TargetStore/ResultsStore must
#                                         not carry an ambient org
#
# The pre-commit hook + CI call this orchestrator by name; touching the
# composition here keeps the caller stable. Unrelated defensive lints
# (PII-in-logs, admin-repo reason, org-audit-log single-writer) live in
# scripts/check_static_lints.sh — wired into the same hook + CI step.

set -euo pipefail

cd "$(dirname "$0")/.."

scripts/check_pg_org_scope.sh
scripts/check_clickhouse_org_scope.sh
scripts/check_safe_comment_syntax.sh
scripts/check_store_construction_org_less.sh
