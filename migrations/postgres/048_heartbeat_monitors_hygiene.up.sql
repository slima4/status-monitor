-- An exit code is a u8 on the way in and read back as one. Without this an
-- out-of-range value truncates, so 256 reads as 0 and a failed run renders as
-- a clean exit.
ALTER TABLE heartbeat_monitors
    ADD CONSTRAINT heartbeat_monitors_exit_code_range
    CHECK (last_exit_code IS NULL OR last_exit_code BETWEEN 0 AND 255);

-- org_id carries ON DELETE CASCADE, and Postgres does not index a referencing
-- column on its own, so an org delete seq-scans this table without it.
CREATE INDEX IF NOT EXISTS idx_heartbeat_monitors_org
    ON heartbeat_monitors(org_id);
