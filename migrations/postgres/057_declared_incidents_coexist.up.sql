-- A declared incident used to occupy the monitor's only open-incident slot, so
-- the writer could neither open nor page a real outage underneath it.
DROP INDEX idx_incidents_org_open;

CREATE UNIQUE INDEX idx_incidents_org_open_monitor
    ON incidents(org_id, target_id) WHERE ended_at IS NULL AND origin = 'monitor';

CREATE UNIQUE INDEX idx_incidents_org_open_manual
    ON incidents(org_id, target_id) WHERE ended_at IS NULL AND origin = 'manual';
