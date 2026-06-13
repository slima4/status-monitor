-- Single-use email-verification tokens for `email` notification channels.
-- Only the token's SHA-256 is stored (same discipline as
-- channel_link_codes.code_hash); presenting the raw token at the public
-- verify endpoint is what proves inbox ownership.
CREATE TABLE channel_verification_tokens (
    id         UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    channel_id UUID NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    -- Address the token verifies; consume re-checks it against the
    -- channel's current config so an address change burns older tokens.
    -- CITEXT matches the rest of the email columns so a case difference
    -- between the mailed address and the channel config doesn't fail verify.
    email      CITEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    used_at    TIMESTAMPTZ
);

-- Send caps count recent mints per channel, per org, and per address.
CREATE INDEX idx_channel_verification_tokens_channel
    ON channel_verification_tokens (channel_id, created_at);
CREATE INDEX idx_channel_verification_tokens_org
    ON channel_verification_tokens (org_id, created_at);
CREATE INDEX idx_channel_verification_tokens_email
    ON channel_verification_tokens (email, created_at);
