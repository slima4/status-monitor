ALTER TABLE plans
    DROP COLUMN IF EXISTS evidence_days,
    DROP COLUMN IF EXISTS max_flow_steps;
