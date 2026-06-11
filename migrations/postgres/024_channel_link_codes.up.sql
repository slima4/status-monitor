-- Single-use codes that let someone outside the dashboard attach exactly
-- one notification channel to an org: `telegram` codes bind a chat via the
-- central bot, `delegate` codes power the public /c/<code> connect page.
-- The consumed row is the only source of the org a channel attaches to;
-- only the code's SHA-256 is stored (same discipline as
-- monitor_shares.token_hash).
CREATE TABLE channel_link_codes (
    id           UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    purpose      TEXT NOT NULL CHECK (purpose IN ('telegram', 'delegate')),
    code_hash    TEXT NOT NULL UNIQUE,
    created_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Name hint for the channel created on consume.
    channel_name TEXT,
    -- Optional pinned channel kind for delegate links; validated app-side
    -- against the creatable kinds, deliberately no CHECK so the transport
    -- list can grow without a migration.
    kind_hint    TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    consumed_at  TIMESTAMPTZ,
    -- Kill switch for delegate links handed to the wrong person.
    revoked_at   TIMESTAMPTZ,
    -- Set on consume; the poll reports expired if it never lands.
    channel_id   UUID REFERENCES notification_channels(id) ON DELETE SET NULL,

    CONSTRAINT channel_link_codes_channel_name_length
        CHECK (channel_name IS NULL OR char_length(channel_name) BETWEEN 1 AND 100)
);

CREATE INDEX idx_channel_link_codes_org ON channel_link_codes (org_id);

-- A connect-OAuth state minted from a delegate link carries the link's id
-- through the dance; the callback's authority is then the link, not a
-- session. Lives here (not 007) because the FK needs this table to exist.
ALTER TABLE oauth_states
    ADD COLUMN link_code_id UUID REFERENCES channel_link_codes(id) ON DELETE CASCADE;
