-- Reverse 018_incident_ops. Best-effort rollback (downs are operator tooling,
-- not a hot path). Manually-declared incidents have no monitor, so they are
-- removed before target_id's NOT NULL is restored.

DROP TABLE IF EXISTS incident_notifications;
DROP TABLE IF EXISTS incident_events;

ALTER TABLE targets
    DROP COLUMN IF EXISTS escalation_policy_id,
    DROP COLUMN IF EXISTS default_severity;

DROP INDEX IF EXISTS idx_incidents_escalation_due;
DROP INDEX IF EXISTS idx_incidents_org_state_started;
DROP INDEX IF EXISTS idx_incidents_org_target_ended;

DELETE FROM incidents WHERE target_id IS NULL;  -- drop manual incidents before restoring NOT NULL
ALTER TABLE incidents DROP CONSTRAINT IF EXISTS incident_monitor_has_target;
ALTER TABLE incidents ALTER COLUMN target_id SET NOT NULL;

ALTER TABLE incidents
    DROP COLUMN IF EXISTS state,
    DROP COLUMN IF EXISTS urgency,
    DROP COLUMN IF EXISTS origin,
    DROP COLUMN IF EXISTS visibility,
    DROP COLUMN IF EXISTS title,
    DROP COLUMN IF EXISTS acknowledged_at,
    DROP COLUMN IF EXISTS acknowledged_by,
    DROP COLUMN IF EXISTS assigned_to,
    DROP COLUMN IF EXISTS resolved_by,
    DROP COLUMN IF EXISTS escalation_policy_id,
    DROP COLUMN IF EXISTS escalation_level,
    DROP COLUMN IF EXISTS escalation_round,
    DROP COLUMN IF EXISTS next_escalation_at;
