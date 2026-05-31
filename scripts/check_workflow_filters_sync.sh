#!/usr/bin/env bash
# ci.yml and release-image.yml must carry an identical `paths-ignore` set, and
# ci.yml's push/pull_request filters must match each other. GitHub Actions
# can't share a YAML anchor across files, so the release-image copy is
# hand-kept — this guards it. Drift in the dangerous direction (a path ignored
# by ci.yml but not release-image.yml) lets release-image build and fire the
# auto-deploy for a commit whose CI was skipped, hanging the deploy ci-gate.
set -euo pipefail

cd "$(dirname "$0")/.."

# Skip (don't fail the commit) when the YAML parser isn't available — mirrors
# the ast-grep gate in .githooks/pre-commit.
if ! python3 -c 'import yaml' 2>/dev/null; then
  printf '\033[1;33m• workflow paths-ignore sync skipped (python3+pyyaml not available)\033[0m\n'
  exit 0
fi

python3 - <<'PY'
import sys, yaml

def filt(path, event):
    doc = yaml.safe_load(open(path))
    on = doc.get(True) or doc.get("on")  # a bare `on:` key parses to YAML True
    return on[event]["paths-ignore"]

ci_push = filt(".github/workflows/ci.yml", "push")
ci_pr   = filt(".github/workflows/ci.yml", "pull_request")
rel      = filt(".github/workflows/release-image.yml", "push")

errs = []
if ci_push != ci_pr:
    errs.append("ci.yml push vs pull_request paths-ignore differ "
                "(the &paths_ignore anchor was broken)")
if ci_push != rel:
    errs.append(
        "ci.yml vs release-image.yml paths-ignore drifted:\n"
        f"    in ci.yml only       : {[x for x in ci_push if x not in rel]}\n"
        f"    in release-image only: {[x for x in rel if x not in ci_push]}"
    )
if errs:
    sys.exit("paths-ignore filters out of sync:\n  " + "\n  ".join(errs))
PY
