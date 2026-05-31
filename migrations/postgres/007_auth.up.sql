-- Authentication schema: sessions, OAuth identities, OAuth round-trip state,
-- API tokens, org invitations, magic-link tokens (schema-only for now), and
-- login audit.

-- DB-backed sessions: cookie hash => user + active org. Lookup happens on
-- every authenticated request; both an idle timeout and an absolute expiry
-- are checked. `id_hash` is the SHA-256 of the cookie value (see
-- `auth::session::hash_session_id`) — storing the raw cookie would let a
-- `sessions`-table leak be replayed as live cookies.
CREATE TABLE sessions (
    id_hash         TEXT PRIMARY KEY,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    active_org_id   UUID REFERENCES organizations(id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL,
    ip_hash         TEXT,
    user_agent_hash TEXT
);
CREATE INDEX idx_sessions_user    ON sessions(user_id);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
-- The daily retention sweep also reaps idle sessions (`last_used_at <
-- now() - idle_timeout`); without this it seq-scans every night.
CREATE INDEX idx_sessions_last_used_at ON sessions(last_used_at);

-- Link rows: one (provider, provider_user_id) pair => one user. PK is the
-- provider pair, NOT user_id, so one user can later own multiple identities
-- (personal + work GitHub) without a migration.
CREATE TABLE oauth_identities (
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Closed enum; keep in lockstep with `auth::OauthProvider::ALL`. Live
    -- drift test (`tests/enum_drift_test.rs`) introspects this CHECK and
    -- fails if the lists disagree.
    provider          TEXT NOT NULL CHECK (provider IN ('github', 'google')),
    provider_user_id  TEXT NOT NULL,
    provider_username TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, provider_user_id)
);
CREATE INDEX idx_oauth_identities_user ON oauth_identities(user_id);

-- Short-lived per-round-trip state for the OAuth dance. Single-use: the
-- callback deletes-and-returns in one statement so concurrent callbacks
-- for the same state can't both proceed. `state_hash` mirrors the
-- `sessions.id_hash` pattern: a leak of this table (slow-query log,
-- pg_stat_statements with literal capture, pg_dump) yields hashes, not
-- replayable tokens.
CREATE TABLE oauth_states (
    state_hash        TEXT PRIMARY KEY,
    -- Same closed-enum invariant as `oauth_identities.provider`.
    provider          TEXT NOT NULL CHECK (provider IN ('github', 'google')),
    redirect_after    TEXT,
    -- Bare invitation id, not the raw token. The token would be a
    -- replayable credential at rest (backup, slow-query log, pg_dump);
    -- the id alone isn't — the accept handler still requires the
    -- caller's session-bound email to match the invitation row's email.
    invitation_id     UUID,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at        TIMESTAMPTZ NOT NULL
);
CREATE INDEX idx_oauth_states_expires ON oauth_states(expires_at);

-- Named API tokens. token_prefix is intentionally NOT UNIQUE — collisions at
-- 48 bits of entropy are vanishingly rare but a UNIQUE constraint would turn
-- the rare event into a user-visible 500. The prefix narrows the lookup set;
-- argon2-verify against token_hash disambiguates.
CREATE TABLE api_tokens (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- NULL = unbound (any member org via X-Uptimepage-Org); non-NULL pins the
    -- token to one org so it can never touch the owner's other orgs.
    org_id          UUID REFERENCES organizations(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    token_hash      TEXT NOT NULL,
    token_prefix    TEXT NOT NULL,
    scopes          JSONB NOT NULL DEFAULT '["full_access"]'::jsonb
                        CHECK (jsonb_typeof(scopes) = 'array'),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at    TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ
);
CREATE INDEX idx_api_tokens_user   ON api_tokens(user_id);
CREATE INDEX idx_api_tokens_prefix ON api_tokens(token_prefix);
CREATE INDEX idx_api_tokens_org    ON api_tokens(org_id) WHERE org_id IS NOT NULL;

-- Single-use org invitations. CITEXT email matches users.email so a
-- mixed-case invite and a verified-lowercase OAuth login resolve to the same
-- recipient. `token_prefix` is the indexed lookup key: the redeem path
-- narrows by prefix (96 bits of entropy in the first 16 base64url chars) and
-- then argon2-verifies the survivor. Without it, redeem would argon2-hash
-- every pending row — a CPU DoS at any meaningful pending count.
CREATE TABLE invitations (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    inviter_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email        CITEXT NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    token_hash   TEXT NOT NULL,
    token_prefix TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    accepted_at  TIMESTAMPTZ,
    declined_at  TIMESTAMPTZ
);
CREATE INDEX idx_invitations_org_pending
    ON invitations(org_id, expires_at)
    WHERE accepted_at IS NULL AND declined_at IS NULL;
CREATE INDEX idx_invitations_email_pending
    ON invitations(email)
    WHERE accepted_at IS NULL AND declined_at IS NULL;
CREATE INDEX idx_invitations_token_prefix
    ON invitations(token_prefix);

-- Magic-link token rows. The request/verify endpoints are gated by
-- `auth.enabled_methods`; until "magic_link" is listed the routes 404 and no
-- rows are ever inserted. `token_prefix` mirrors the invitations pattern:
-- redeem narrows by indexed prefix and argon2-verifies the survivor.
CREATE TABLE magic_link_tokens (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email        CITEXT NOT NULL,
    token_hash   TEXT NOT NULL,
    token_prefix TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    used_at      TIMESTAMPTZ,
    ip_hash      TEXT
);
CREATE INDEX idx_magic_link_tokens_unused
    ON magic_link_tokens(email)
    WHERE used_at IS NULL;
CREATE INDEX idx_magic_link_tokens_expires
    ON magic_link_tokens(expires_at);
CREATE INDEX idx_magic_link_tokens_prefix
    ON magic_link_tokens(token_prefix)
    WHERE used_at IS NULL;

-- Audit of every authentication attempt — successes for the user's "recent
-- activity" page, failures for credential-stuffing detection.
CREATE TABLE login_attempts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID REFERENCES users(id) ON DELETE SET NULL,
    method          TEXT NOT NULL,
    success         BOOLEAN NOT NULL,
    ip_hash         TEXT,
    user_agent_hash TEXT,
    failure_reason  TEXT,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_login_attempts_user_time
    ON login_attempts(user_id, occurred_at DESC);
-- Partial index restricted to failures. Postgres rejects `now() - interval
-- '7 days'` in an index predicate (now() is not IMMUTABLE) so callers must
-- supply the time window at query time; the planner still uses this for
-- selectivity.
CREATE INDEX idx_login_attempts_failures_by_ip
    ON login_attempts(occurred_at DESC, ip_hash)
    WHERE success = false;
-- Full (non-partial) occurred_at index for the daily retention delete: the
-- partial index above only covers failures, so an unfiltered
-- `occurred_at < cutoff` delete would otherwise seq-scan the whole table.
CREATE INDEX idx_login_attempts_occurred_at ON login_attempts(occurred_at);
