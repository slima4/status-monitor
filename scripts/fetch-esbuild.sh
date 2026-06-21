#!/usr/bin/env bash
# Fetch the esbuild standalone binary for the current host.
# Usage: scripts/fetch-esbuild.sh [version]
# Downloads to ./bin/esbuild (gitignored).

set -euo pipefail

VERSION="${1:-0.28.1}"
BIN_DIR="$(cd "$(dirname "$0")/.." && pwd)/bin"
TARGET="$BIN_DIR/esbuild"

mkdir -p "$BIN_DIR"

if [[ -x "$TARGET" ]]; then
    echo "esbuild already present at $TARGET"
    exit 0
fi

uname_s="$(uname -s)"
uname_m="$(uname -m)"

# esbuild ships per-platform npm packages whose Go binary is statically linked,
# so one Linux build covers glibc and musl alike — no libc suffix needed.
case "$uname_s-$uname_m" in
    Darwin-arm64)   pkg="darwin-arm64" ;;
    Darwin-x86_64)  pkg="darwin-x64" ;;
    Linux-aarch64)  pkg="linux-arm64" ;;
    Linux-x86_64)   pkg="linux-x64" ;;
    *) echo "unsupported platform: $uname_s-$uname_m" >&2; exit 1 ;;
esac

url="https://registry.npmjs.org/@esbuild/${pkg}/-/${pkg}-${VERSION}.tgz"
echo "fetching $url"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fL --retry 3 -o "$tmp/esbuild.tgz" "$url"
tar -xzf "$tmp/esbuild.tgz" -C "$tmp" package/bin/esbuild
mv "$tmp/package/bin/esbuild" "$TARGET"
chmod +x "$TARGET"
echo "installed $TARGET"
