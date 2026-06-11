-- Single-use link codes for the central Telegram bot. Minted by the channel
-- form, carried to Telegram inside a t.me deep link, and consumed by the
-- webhook when the chat sends it back via /start or /link — the consumed row
-- is the only source of the org a chat links to. Only the SHA-256 of the code
-- is stored (same discipline as monitor_shares.token_hash).
CREATE TABLE telegram_link_codes (
    id           UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    code_hash    TEXT NOT NULL UNIQUE,
    -- Audit-only; the code keeps working if the creator leaves the org.
    created_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Optional name hint from the form for the channel created on consume.
    channel_name TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    consumed_at  TIMESTAMPTZ,
    -- The channel the consume created; stays readable for the status poll
    -- even if the channel is later deleted (poll then reports expired).
    channel_id   UUID REFERENCES notification_channels(id) ON DELETE SET NULL,

    CONSTRAINT telegram_link_codes_channel_name_length
        CHECK (channel_name IS NULL OR char_length(channel_name) BETWEEN 1 AND 100)
);

-- Outstanding-codes cap counts per org; the UNIQUE code_hash covers consume.
CREATE INDEX idx_telegram_link_codes_org ON telegram_link_codes (org_id);
