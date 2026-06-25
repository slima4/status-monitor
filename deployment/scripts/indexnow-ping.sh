#!/usr/bin/env bash
# Notify IndexNow (Bing; DuckDuckGo via Bing) that the marketing URLs
# changed, so they recrawl sooner. Google does not participate. Advisory: the
# sitemap is the durable signal, so a failed ping is not fatal.
#
# The host must also serve the ownership proof at /<key>.txt (a Caddy respond,
# set up separately). Engines fetch it to verify the key before accepting a
# ping; this script warns if it is not reachable.
#
#   INDEXNOW_KEY=<key> [ORIGIN=https://uptimepage.dev] deployment/scripts/indexnow-ping.sh
set -euo pipefail

ORIGIN="${ORIGIN:-https://uptimepage.dev}"
ORIGIN="${ORIGIN%/}"
KEY="${INDEXNOW_KEY:?set INDEXNOW_KEY (the key served at /<key>.txt)}"
ENDPOINT="https://api.indexnow.org/indexnow"

for dep in curl jq; do
  command -v "$dep" >/dev/null || { echo "missing dependency: $dep" >&2; exit 1; }
done

host="${ORIGIN#http://}"; host="${host#https://}"; host="${host%%/*}"
key_location="${ORIGIN}/${KEY}.txt"

if [ "$(curl -sS --connect-timeout 5 --max-time 10 -o /dev/null -w '%{http_code}' "$key_location" || true)" != "200" ]; then
  echo "warning: ${key_location} is not serving 200; engines will reject the ping" >&2
fi

# The live sitemap is the single source of URLs — no duplicated list here.
sitemap="$(curl -fsS --connect-timeout 10 --max-time 30 "${ORIGIN}/sitemap.xml")" || { echo "failed to fetch ${ORIGIN}/sitemap.xml" >&2; exit 1; }
# Strip the tags and decode &amp; (sitemap <loc> values are XML-escaped).
mapfile -t urls < <(printf '%s' "$sitemap" | grep -oE '<loc>[^<]+</loc>' | sed -E 's#</?loc>##g; s/&amp;/\&/g')
if [ "${#urls[@]}" -eq 0 ]; then
  echo "no <loc> URLs found in ${ORIGIN}/sitemap.xml" >&2; exit 1
fi

payload="$(jq -n \
  --arg host "$host" \
  --arg key "$KEY" \
  --arg loc "$key_location" \
  '{host: $host, key: $key, keyLocation: $loc, urlList: $ARGS.positional}' \
  --args "${urls[@]}")"

code="$(curl -sS --connect-timeout 10 --max-time 30 -o /dev/null -w '%{http_code}' \
  -X POST "$ENDPOINT" \
  -H 'Content-Type: application/json; charset=utf-8' \
  --data "$payload" || true)"

case "$code" in
  200|202) echo "IndexNow: submitted ${#urls[@]} URLs (HTTP $code)" ;;
  *)       echo "IndexNow: submission failed (HTTP $code)" >&2; exit 1 ;;
esac
