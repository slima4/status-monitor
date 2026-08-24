-- A pending heartbeat is silent by design, so one nobody finishes wiring stays
-- quiet forever. This column is what keeps the reminder to exactly one.
ALTER TABLE heartbeat_monitors
    ADD COLUMN IF NOT EXISTS nudged_at TIMESTAMPTZ;

-- Keeps the sweep off the wired majority.
CREATE INDEX IF NOT EXISTS idx_heartbeat_monitors_unwired
    ON heartbeat_monitors(created_at)
    WHERE first_ping_at IS NULL AND nudged_at IS NULL;
