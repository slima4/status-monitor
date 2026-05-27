#!/usr/bin/env bash
# Forbid `-- SAFE:` line comments inside any source file. A line-comment
# annotation inside a Rust string literal that uses `\` line-continuation
# silently folds into a single line at compile time, and the `--` comment
# then swallows the actual SQL — turning the statement into a no-op that
# the runtime can't distinguish from "matched zero rows".
#
# Always use the block form: `/* SAFE: <reason> */`. The terminator is
# explicit and survives whitespace collapse.

set -euo pipefail

cd "$(dirname "$0")/.."

broken="$(rg --pcre2 --line-number -e '-- SAFE:' src/ || true)"
if [ -n "$broken" ]; then
  echo "check_safe_comment_syntax failed: '-- SAFE:' line comment in SQL literal." >&2
  echo "Rust line-continuation '\\' eats the newline; PG sees one line and the comment swallows the statement." >&2
  echo "Use '/* SAFE: ... */' block comments instead." >&2
  echo >&2
  echo "$broken" >&2
  exit 1
fi
