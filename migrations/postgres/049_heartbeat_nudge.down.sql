DROP INDEX IF EXISTS idx_heartbeat_monitors_unwired;
ALTER TABLE heartbeat_monitors DROP COLUMN IF EXISTS nudged_at;
