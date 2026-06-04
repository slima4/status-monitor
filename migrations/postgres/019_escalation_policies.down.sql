ALTER TABLE plans DROP COLUMN IF EXISTS max_escalation_policies;

DROP INDEX IF EXISTS idx_targets_escalation_policy;
ALTER TABLE targets DROP CONSTRAINT IF EXISTS fk_targets_escalation_policy;
ALTER TABLE incidents DROP CONSTRAINT IF EXISTS fk_incidents_escalation_policy;
ALTER TABLE organizations DROP COLUMN IF EXISTS default_escalation_policy_id;

DROP TABLE IF EXISTS escalation_targets;
DROP TABLE IF EXISTS escalation_steps;
DROP TABLE IF EXISTS escalation_policies;
