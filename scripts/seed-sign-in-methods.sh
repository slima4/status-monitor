#!/usr/bin/env bash
# Seed sign-in methods for the dev operator so /settings/account renders the
# full card: two linked providers (so both are removable), a third that is
# linked but whose provider is switched off, and a credential trail covering
# every origin.
#
# `just dev-login` mints the user + session but no OAuth identity, so without
# this the card is empty and none of the paths this exercises are reachable.
#
# Idempotent: the seeded rows are wiped and re-inserted.
#
# Env overrides:
#   EMAIL         operator email          (default: dev@local.test)
#   PG_CONTAINER  postgres container name (default: uptimepage-postgres-1)
#   PG_DB         database name           (default: monitor)
set -euo pipefail

EMAIL="${EMAIL:-dev@local.test}"
PG_CONTAINER="${PG_CONTAINER:-uptimepage-postgres-1}"
PG_DB="${PG_DB:-monitor}"

pg() { docker exec -i "$PG_CONTAINER" psql -U monitor -d "$PG_DB" -v ON_ERROR_STOP=1 "$@"; }

pg -q <<SQL
DO \$\$
DECLARE uid UUID;
BEGIN
    SELECT id INTO uid FROM users WHERE email = '${EMAIL}'::citext;
    IF uid IS NULL THEN
        RAISE EXCEPTION 'no user %, run: just dev-login', '${EMAIL}';
    END IF;

    DELETE FROM oauth_identities WHERE user_id = uid;
    DELETE FROM credential_events WHERE user_id = uid;

    INSERT INTO oauth_identities
        (user_id, provider, provider_user_id, provider_username, created_at, last_login_at)
    VALUES
        (uid, 'github', 'dev-gh-1', 'dev-operator', now() - INTERVAL '90 days', now()),
        (uid, 'gitlab', 'https://gitlab.com/4242', 'dev-at-work', now() - INTERVAL '9 days', now() - INTERVAL '2 days'),
        (uid, 'google', 'dev-goog-1', 'dev@local.test', now() - INTERVAL '3 days', now() - INTERVAL '3 days');

    INSERT INTO credential_events
        (user_id, provider, provider_user_id, action, origin, ip_hash, occurred_at)
    VALUES
        (uid, 'github', 'dev-gh-1', 'linked', 'signup', 'devhash', now() - INTERVAL '90 days'),
        (uid, 'gitlab', 'https://gitlab.com/4242', 'linked', 'session', 'devhash', now() - INTERVAL '9 days'),
        (uid, 'google', 'dev-goog-1', 'linked', 'email_match', 'otherhash', now() - INTERVAL '3 days'),
        (uid, 'microsoft', 'dev-ms-gone', 'unlinked', 'session', 'devhash', now() - INTERVAL '1 day');
END
\$\$;
SQL

echo
echo "Seeded 3 sign-in methods + 4 credential events for ${EMAIL}."
echo
echo "  /settings/account          the card: three methods, each removable"
echo "  DELETE one                 mails (to the log), audits, revokes your"
echo "                             other sessions — reload to see it gone"
echo "  /api/v1/me/data-export     credential_changes carries the trail,"
echo "                             including the microsoft one already removed"
echo
echo "To reach the last-method guard, remove two, then try the third with"
echo "magic_link off (UPTIMEPAGE_AUTH__ENABLED_METHODS='[\"github_oauth\"]')."
