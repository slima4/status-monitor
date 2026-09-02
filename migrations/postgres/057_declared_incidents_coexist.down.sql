DROP INDEX idx_incidents_org_open_manual;
DROP INDEX idx_incidents_org_open_monitor;

CREATE UNIQUE INDEX idx_incidents_org_open
    ON incidents(org_id, target_id) WHERE ended_at IS NULL;
