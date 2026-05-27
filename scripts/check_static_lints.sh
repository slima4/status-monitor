#!/usr/bin/env bash
# Defensive ast-grep lints unrelated to tenant isolation:
#
#   admin_repo_static_reason       AdminRepo::new's audit-trail reason
#                                  must be a string literal (audit grep
#                                  enumerates every cross-tenant entry).
#   no_unredacted_pii_in_logs      tracing! macros must not interpolate
#                                  raw IP/email/url/token/secret; use
#                                  `// SAFE:` to opt out a specific call.
#   org_audit_single_writer        only `storage::orgs::record_audit_tx`
#                                  may INSERT INTO org_audit_log.
#
# Tenant-isolation rule (`no_raw_tenant_sql`) is intentionally NOT
# included — it has file-level allow-lists and a per-statement escape
# hatch that this script's broad scan would not respect. See
# scripts/check_pg_org_scope.sh.

set -euo pipefail

cd "$(dirname "$0")/.."

ast-grep scan --rule scripts/sg-rules/admin_repo_static_reason.yml src/
ast-grep scan --rule scripts/sg-rules/no_unredacted_pii_in_logs.yml src/
ast-grep scan --rule scripts/sg-rules/org_audit_single_writer.yml src/
