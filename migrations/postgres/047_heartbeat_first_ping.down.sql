-- Incidents closed by the up migration stay closed: reopening a withdrawn
-- false alarm would page every owner a second time.
ALTER TABLE heartbeat_monitors DROP COLUMN IF EXISTS first_ping_at;
