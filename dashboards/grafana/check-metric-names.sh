#!/usr/bin/env bash
# Metric-name drift gate. Fails (non-zero) if:
#   1. a dashboard panel references a status_monitor_* name absent from the binary
#   2. a status_monitor_* name in docs/metrics.md is absent from the binary
#   3. a metric name registered in src/observability/metrics.rs is missing
#      from the docs/metrics.md series table
# Run from anywhere; paths resolve relative to the repo root.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

dash="$root/terraform/dashboards/status-monitor-overview.json"
metrics_rs="$root/src/observability/metrics.rs"
metrics_md="$root/docs/metrics.md"

fail=0
emit() { echo "DRIFT: $*" >&2; fail=1; }

# Names the binary actually knows: quoted "status_monitor_*" literals in
# metrics.rs (the `names` consts, describe_* calls, and build_info).
mapfile -t code_names < <(grep -oE '"status_monitor_[a-z_]+"' "$metrics_rs" | tr -d '"' | sort -u)

contains() { local n="$1"; shift; local x; for x in "$@"; do [[ "$x" == "$n" ]] && return 0; done; return 1; }

# 1. dashboard -> binary
while read -r n; do
  contains "$n" "${code_names[@]}" || emit "panel uses '$n' — not registered in src/observability/metrics.rs"
done < <(grep -oE 'status_monitor_[a-z_]+' "$dash" | sort -u)

# 2. docs -> binary
while read -r n; do
  contains "$n" "${code_names[@]}" || emit "docs/metrics.md lists '$n' — not registered in src/observability/metrics.rs"
done < <(grep -oE 'status_monitor_[a-z_]+' "$metrics_md" | sort -u)

# 3. binary -> docs
for n in "${code_names[@]}"; do
  grep -q "$n" "$metrics_md" || emit "metric '$n' registered in code but missing from docs/metrics.md"
done

if [[ $fail -eq 0 ]]; then
  echo "metric names: dashboard <-> code <-> docs all consistent"
fi
exit $fail
