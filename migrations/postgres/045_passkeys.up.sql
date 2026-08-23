CREATE TABLE webauthn_credentials (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Unique across every account, not just within one: a discoverable login
    -- hands this back with no hint of whose it is.
    credential_id BYTEA NOT NULL UNIQUE,
    -- webauthn-rs owns the shape and moves the counter inside it, so the whole
    -- value round-trips rather than being unpacked.
    credential    JSONB NOT NULL,
    -- A passkey only answers to the id that minted it, so this names the rows a
    -- changed public_base_url orphaned.
    rp_id         TEXT NOT NULL,
    nickname      TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_webauthn_credentials_user ON webauthn_credentials(user_id);

-- Not a row in `oauth_states`: that table's provider CHECK is the OAuth enum
-- and a passkey ceremony names no provider.
CREATE TABLE webauthn_states (
    state_hash TEXT PRIMARY KEY,
    -- Set when a signed-in session started the dance, so the credential lands
    -- on that account. NULL is a login, where the authenticator names the user.
    user_id    UUID REFERENCES users(id) ON DELETE CASCADE,
    -- Single-use and short-lived, which is what makes serialising the library's
    -- ceremony state safe to keep at all.
    state      JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_webauthn_states_expires ON webauthn_states(expires_at);
-- The cascade from `users` seq-scans without this, same as `sessions`.
CREATE INDEX idx_webauthn_states_user ON webauthn_states(user_id) WHERE user_id IS NOT NULL;
