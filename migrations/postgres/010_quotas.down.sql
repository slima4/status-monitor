-- Reverse order of 010_quotas.up.sql. quota_events / plan_overrides reference
-- organizations + users (not plans); organizations.plan_id references plans,
-- so the FK column goes before the plans table.
DROP TABLE IF EXISTS quota_events;
DROP TABLE IF EXISTS org_addons;
DROP TABLE IF EXISTS plan_overrides;

DROP INDEX IF EXISTS idx_organizations_plan;
ALTER TABLE organizations DROP COLUMN IF EXISTS plan_id;

DROP TABLE IF EXISTS plans;
