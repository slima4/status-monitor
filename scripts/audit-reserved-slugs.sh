#!/usr/bin/env bash
# Audit live DNS labels under `${UPTIMEPAGE_DOMAIN}` against
# `src/domain/reserved_slugs.rs`. Apex-wildcard mode means every operator
# subdomain shares its parent with `*.${domain}` — any label that resolves
# but is NOT in the reserved list could be claimed by a user and stand in
# for the real subdomain.
#
# Fails (exit 1) if any label resolves but is not reserved.
#
# Required:
#   UPTIMEPAGE_DOMAIN — the apex domain, e.g. uptimepage.dev
#
# Optional:
#   SUBDOMAIN_PROBES — space-separated extra labels to dig explicitly. The
#                      default list covers the common operator / marketing /
#                      asset / email surfaces. The script ONLY queries the
#                      labels in this list; it does NOT enumerate the apex
#                      zone (that would require a zone-transfer or the
#                      provider's DNS API).

set -euo pipefail

cd "$(dirname "$0")/.."

domain="${UPTIMEPAGE_DOMAIN:?UPTIMEPAGE_DOMAIN must be set}"
reserved_file="src/domain/reserved_slugs.rs"
probes="${SUBDOMAIN_PROBES:-app www mail blog docs api status assets cdn static media images img logo www2 wwww mail2 smtp imap pop autodiscover autoconfig dkim dmarc spf _acme-challenge}"

# Resolve the reserved set once instead of re-grepping per label.
reserved="$(grep -oE '"[a-z0-9-]+"' "$reserved_file" | tr -d '"' | sort -u)"

missing=0
for sub in $probes; do
  # `dig +short` prints one line per RDATA record. Empty output = NXDOMAIN
  # / NODATA. A single dot ("."), or any non-empty answer, means the label
  # is live and routable.
  if [ -n "$(dig +short "$sub.$domain" 2>/dev/null)" ] \
     || [ -n "$(dig +short "$sub.$domain" AAAA 2>/dev/null)" ] \
     || [ -n "$(dig +short "$sub.$domain" CNAME 2>/dev/null)" ]; then
    if ! echo "$reserved" | grep -qx "$sub"; then
      echo "MISSING reservation for: $sub.$domain"
      missing=$((missing + 1))
    fi
  fi
done

if [ "$missing" -gt 0 ]; then
  echo "audit-reserved-slugs.sh: $missing label(s) resolve but are not in $reserved_file" >&2
  exit 1
fi

echo "audit-reserved-slugs.sh: all resolving probe labels are reserved."
