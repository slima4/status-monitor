DROP INDEX IF EXISTS idx_status_pages_plan_hold;
DROP INDEX IF EXISTS idx_targets_plan_hold;

ALTER TABLE status_pages DROP COLUMN IF EXISTS plan_keep;
ALTER TABLE targets DROP COLUMN IF EXISTS plan_keep;
ALTER TABLE status_pages DROP COLUMN IF EXISTS plan_hold_at;
ALTER TABLE targets DROP COLUMN IF EXISTS plan_hold_at;
