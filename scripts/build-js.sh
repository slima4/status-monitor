#!/usr/bin/env bash
# Bundle authored JS (assets/js) into static/js with esbuild — the single source
# of truth for entry selection + flags, shared by build.rs and `just watch-js`.
# Usage: scripts/build-js.sh [build|watch]    (PROFILE env: debug|release)
set -euo pipefail
cd "$(dirname "$0")/.."

mode="${1:-build}"

[[ -x bin/esbuild ]] || bash scripts/fetch-esbuild.sh

# bash 3.2 (macOS default) has no `mapfile -d`, so read the NUL-delimited list.
entries=()
while IFS= read -r -d '' f; do entries+=("$f"); done \
    < <(find assets/js -name '*.js' ! -name '_*' -print0)
[[ ${#entries[@]} -gt 0 ]] || { echo "no JS entries under assets/js" >&2; exit 1; }

# esbuild never removes stale outputs; clear every bundle subdir (vendored libs
# are top-level files under static/js and stay put).
find static/js -mindepth 1 -maxdepth 1 -type d -exec rm -rf {} +
rm -f static/js/manifest.json static/js/.esbuild-meta.json

common=(--bundle --sourcemap=linked --format=iife --outbase=assets/js --outdir=static/js)

if [[ $mode == watch ]]; then
    exec ./bin/esbuild "${entries[@]}" "${common[@]}" \
        --entry-names='[dir]/[name]' --watch=forever
fi

# Release content-hashes names + writes a metafile (build.rs turns it into the
# manifest). Debug uses stable names served via the per-request fingerprint.
if [[ "${PROFILE:-debug}" == release ]]; then
    ./bin/esbuild "${entries[@]}" "${common[@]}" --minify \
        --entry-names='[dir]/[name]-[hash]' --metafile=static/js/.esbuild-meta.json
else
    ./bin/esbuild "${entries[@]}" "${common[@]}" --entry-names='[dir]/[name]'
fi
