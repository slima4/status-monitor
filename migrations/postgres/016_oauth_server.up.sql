-- OAuth 2.1 Authorization Server + audience-bound access tokens for the MCP
-- connector. Additive: prod is live, so no existing table is rewritten.

-- Audience (RFC 8707) + issuing client for OAuth-minted access tokens. Both
-- NULL for manually-minted (`sm_live_`) tokens, which keeps the manual path and
-- its per-user cap unchanged. The MCP resource server requires `audience` to
-- match its canonical URI when present.
ALTER TABLE api_tokens ADD COLUMN audience TEXT;
ALTER TABLE api_tokens ADD COLUMN oauth_client_id TEXT;

-- One row per OAuth-minted connector token, keyed by issuing client, so a
-- re-consent can revoke the prior token for the same (user, org, client,
-- audience) tuple before minting the next.
CREATE INDEX idx_api_tokens_oauth_client
    ON api_tokens (user_id, org_id, oauth_client_id, audience)
    WHERE oauth_client_id IS NOT NULL;

-- Dynamically-registered OAuth clients (RFC 7591). Public clients (PKCE, no
-- secret); `redirect_uris` is the exact-match allow-list.
CREATE TABLE oauth_clients (
    client_id      TEXT PRIMARY KEY,
    client_name    TEXT,
    redirect_uris  JSONB NOT NULL,
    grant_types    JSONB NOT NULL DEFAULT '["authorization_code"]'::jsonb,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Short-lived, single-use authorization codes. `code_hash` is the SHA-256 of
-- the high-entropy code (the code itself is never stored). Bound to the client,
-- the exact redirect_uri, the PKCE challenge, the resource (audience), and the
-- consenting user + org. Consumed via DELETE-RETURNING so it can never replay.
CREATE TABLE oauth_authorization_codes (
    code_hash       TEXT PRIMARY KEY,
    client_id       TEXT NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
    redirect_uri    TEXT NOT NULL,
    code_challenge  TEXT NOT NULL,
    scope           TEXT NOT NULL,
    resource        TEXT NOT NULL,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Code lifetime (~60s). Distinct from the token lifetimes below.
    expires_at      TIMESTAMPTZ NOT NULL,
    -- Connection lifetime chosen by the user at consent: how long the refresh
    -- token (and thus the connection) stays valid. Carried to the token
    -- endpoint. The access token itself is short-lived + auto-renewed.
    refresh_expires_at TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Sweep helper: expired codes are dead weight once past `expires_at`.
CREATE INDEX idx_oauth_codes_expires ON oauth_authorization_codes (expires_at);

-- Rotating refresh tokens (OAuth 2.1). `token_hash` is the SHA-256 of the
-- refresh token (never stored raw). Single-use: each refresh rotates to a new
-- token in the same `family_id`. A rotated row is kept with `used_at` set so a
-- replay of an already-used token is detected as theft — the whole family is
-- then revoked. Bound to client/scope/resource/user/org so a refresh can never
-- widen scope, cross orgs, or change audience.
CREATE TABLE oauth_refresh_tokens (
    token_hash   TEXT PRIMARY KEY,
    family_id    UUID NOT NULL,
    client_id    TEXT NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
    scope        TEXT NOT NULL,
    resource     TEXT NOT NULL,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- NULL = current/valid; non-NULL = already rotated (replay ⇒ compromise).
    used_at      TIMESTAMPTZ,
    expires_at   TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Family revocation on replay + expiry sweep.
CREATE INDEX idx_oauth_refresh_family  ON oauth_refresh_tokens (family_id);
CREATE INDEX idx_oauth_refresh_expires ON oauth_refresh_tokens (expires_at);
