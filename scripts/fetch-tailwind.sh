#!/usr/bin/env bash
# Fetch the Tailwind 4 standalone CLI for the current host.
# Usage: scripts/fetch-tailwind.sh [version]
# Downloads to ./bin/tailwindcss (gitignored).

set -euo pipefail

VERSION="${1:-v4.3.0}"
BIN_DIR="$(cd "$(dirname "$0")/.." && pwd)/bin"
TARGET="$BIN_DIR/tailwindcss"

mkdir -p "$BIN_DIR"

if [[ -x "$TARGET" ]]; then
    echo "tailwindcss already present at $TARGET"
    exit 0
fi

uname_s="$(uname -s)"
uname_m="$(uname -m)"

# Detect musl libc (Alpine, etc.) so we pick the matching upstream asset.
# The non-musl Linux build is glibc-linked and won't run on Alpine even with
# gcompat present (Tailwind/Bun's link layout is sensitive to it).
libc_suffix=""
if [[ "$uname_s" == "Linux" ]] && (ldd --version 2>&1 || true) | grep -qi musl; then
    libc_suffix="-musl"
fi

case "$uname_s-$uname_m" in
    Darwin-arm64)   asset="tailwindcss-macos-arm64" ;;
    Darwin-x86_64)  asset="tailwindcss-macos-x64" ;;
    Linux-aarch64)  asset="tailwindcss-linux-arm64${libc_suffix}" ;;
    Linux-x86_64)   asset="tailwindcss-linux-x64${libc_suffix}" ;;
    *) echo "unsupported platform: $uname_s-$uname_m" >&2; exit 1 ;;
esac

url="https://github.com/tailwindlabs/tailwindcss/releases/download/${VERSION}/${asset}"
echo "fetching $url"
curl -fL --retry 3 -o "$TARGET" "$url"
chmod +x "$TARGET"
echo "installed $TARGET"
