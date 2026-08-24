DROP INDEX IF EXISTS idx_heartbeat_monitors_org;
ALTER TABLE heartbeat_monitors DROP CONSTRAINT IF EXISTS heartbeat_monitors_exit_code_range;
