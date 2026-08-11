-- The nightly purge bounds this table by created_at alone; the org index leads
-- on org_id, so the sweep had nothing to walk.
CREATE INDEX idx_mcp_audit_created ON mcp_audit (created_at);
