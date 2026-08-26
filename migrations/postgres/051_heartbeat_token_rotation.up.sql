-- Same row so history survives. No sealed copy of the superseded token: it is
-- accepted until it expires, never re-displayed.
ALTER TABLE heartbeat_monitors
    ADD COLUMN IF NOT EXISTS token_rotated_at        TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS prev_token_hash         TEXT,
    ADD COLUMN IF NOT EXISTS prev_token_expires_at   TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS prev_token_last_used_at TIMESTAMPTZ;

-- A prev hash with no expiry would be accepted forever.
ALTER TABLE heartbeat_monitors
    ADD CONSTRAINT heartbeat_monitors_prev_token_shape
    CHECK ((prev_token_hash IS NULL) = (prev_token_expires_at IS NULL)
           AND (prev_token_last_used_at IS NULL OR prev_token_hash IS NOT NULL));

-- Ping resolution ORs this against the token_hash unique index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_heartbeat_monitors_prev_token
    ON heartbeat_monitors(prev_token_hash) WHERE prev_token_hash IS NOT NULL;
