DROP INDEX IF EXISTS idx_heartbeat_monitors_prev_token;
ALTER TABLE heartbeat_monitors DROP CONSTRAINT IF EXISTS heartbeat_monitors_prev_token_shape;
ALTER TABLE heartbeat_monitors
    DROP COLUMN IF EXISTS token_rotated_at,
    DROP COLUMN IF EXISTS prev_token_hash,
    DROP COLUMN IF EXISTS prev_token_expires_at,
    DROP COLUMN IF EXISTS prev_token_last_used_at;
