-- A plan that shrinks under an account leaves it over cap. Rather than delete
-- what no longer fits, the excess is held: the row survives untouched and
-- stops being served, so the plan coming back restores it exactly.
--
-- Distinct from `enabled`, which is the customer's own switch. A held monitor
-- that the customer had also paused must come back paused, and re-using
-- `enabled` would lose that. Same reasoning as
-- `notification_channels.disabled_reason`.

ALTER TABLE targets ADD COLUMN plan_hold_at TIMESTAMPTZ;
ALTER TABLE status_pages ADD COLUMN plan_hold_at TIMESTAMPTZ;

-- Holds exist only on an account that has shrunk, so both scans below are
-- empty on nearly every install. The partial indexes keep "does this account
-- hold anything" free for the fleet that holds nothing, which is the same
-- pre-filter shape the maintenance release scan uses.
CREATE INDEX idx_targets_plan_hold
    ON targets(org_id) WHERE plan_hold_at IS NOT NULL;
CREATE INDEX idx_status_pages_plan_hold
    ON status_pages(org_id) WHERE plan_hold_at IS NOT NULL;

-- The customer's own answer to "which of these matters". Stored rather than
-- passed in, because reconciliation is recomputed from scratch on every run:
-- a pick that lived only in the request would be reverted by the next sweep,
-- putting back on hold the very monitor they asked to keep.
ALTER TABLE targets ADD COLUMN plan_keep BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE status_pages ADD COLUMN plan_keep BOOLEAN NOT NULL DEFAULT false;
