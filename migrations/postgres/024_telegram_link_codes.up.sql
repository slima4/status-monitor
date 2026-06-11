-- Single-use codes binding a Telegram chat to an org via the central bot.
-- The consumed row is the only source of the org a chat links to; only the
-- code's SHA-256 is stored (same discipline as monitor_shares.token_hash).
CREATE TABLE telegram_link_codes (
    id           UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    code_hash    TEXT NOT NULL UNIQUE,
    created_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Name hint for the channel created on consume.
    channel_name TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    consumed_at  TIMESTAMPTZ,
    -- Set on consume; the poll reports expired if it never lands.
    channel_id   UUID REFERENCES notification_channels(id) ON DELETE SET NULL,

    CONSTRAINT telegram_link_codes_channel_name_length
        CHECK (channel_name IS NULL OR char_length(channel_name) BETWEEN 1 AND 100)
);

CREATE INDEX idx_telegram_link_codes_org ON telegram_link_codes (org_id);
