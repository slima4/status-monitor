#!/usr/bin/env bash
# Operator target/results repos must stay org-less at construction: the
# `org` is a per-call argument resolved from the request's `CurrentOrg`,
# so "forgot to scope" is a compile error rather than a cross-tenant
# leak. Re-adding an ambient org to the constructor (a third arg to
# `PostgresTargetStore::from_pool` / a second to
# `ClickhouseResultsStore::from_client`) silently re-opens the IDOR; the
# type system can't catch that, so grep does.
#
# `ClickhouseResultSink` (write side) legitimately keeps its org and is
# intentionally not matched here.

set -euo pipefail

cd "$(dirname "$0")/.."

ambient="$(rg --pcre2 --multiline --line-number \
  -e 'PostgresTargetStore::from_pool\(\s*[^,]+,\s*[^,)]+,\s*[^)]+\)' \
  -e 'ClickhouseResultsStore::from_client\(\s*[^,)]+,\s*[^)]+\)' \
  src/ tests/ benches/ || true)"

if [ -n "$ambient" ]; then
  echo "check_store_construction_org_less failed: target/results store constructed with an ambient org." >&2
  echo "Drop the org arg; pass it per call from CurrentOrg instead." >&2
  echo >&2
  echo "$ambient" >&2
  exit 1
fi
