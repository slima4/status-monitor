ALTER TABLE plans DROP COLUMN IF EXISTS on_call_enabled;
ALTER TABLE plans DROP COLUMN IF EXISTS max_on_call_schedules;

DROP TABLE IF EXISTS user_contact_channels;

-- Second-axis org backstop on escalation_targets was added here (it references
-- on_call_schedules); remove it before the schedule FK + table go.
DROP TRIGGER IF EXISTS trg_escalation_targets_refs_org ON escalation_targets;
DROP FUNCTION IF EXISTS assert_escalation_target_refs_org();
ALTER TABLE escalation_targets DROP CONSTRAINT IF EXISTS fk_escalation_targets_schedule;

DROP TABLE IF EXISTS on_call_overrides;
DROP TABLE IF EXISTS on_call_participants;
DROP TABLE IF EXISTS on_call_layers;
DROP TABLE IF EXISTS on_call_schedules;
