-- Put the plan back on the org. Each org takes its account's plan, so an
-- account that held several orgs hands the same plan to each of them: the
-- pool splits back into per-org caps.
ALTER TABLE organizations ADD COLUMN plan_id TEXT REFERENCES plans(id);
UPDATE organizations o SET plan_id = a.plan_id FROM accounts a WHERE a.id = o.account_id;
ALTER TABLE organizations ALTER COLUMN plan_id SET NOT NULL;
ALTER TABLE organizations ALTER COLUMN plan_id SET DEFAULT 'free';
CREATE INDEX idx_organizations_plan ON organizations(plan_id);

ALTER TABLE account_addons RENAME TO org_addons;
ALTER TABLE org_addons ADD COLUMN org_id UUID REFERENCES organizations(id) ON DELETE CASCADE;
-- One org per account row: the account's oldest org takes the add-on.
UPDATE org_addons a
   SET org_id = (SELECT o.id FROM organizations o
                  WHERE o.account_id = a.account_id
                  ORDER BY o.created_at ASC, o.id ASC LIMIT 1);
DELETE FROM org_addons WHERE org_id IS NULL;
ALTER TABLE org_addons ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE org_addons DROP COLUMN account_id;
ALTER TABLE org_addons ADD PRIMARY KEY (org_id, addon_type);

ALTER TABLE plan_overrides ADD COLUMN org_id UUID REFERENCES organizations(id) ON DELETE CASCADE;
UPDATE plan_overrides po
   SET org_id = (SELECT o.id FROM organizations o
                  WHERE o.account_id = po.account_id
                  ORDER BY o.created_at ASC, o.id ASC LIMIT 1);
DELETE FROM plan_overrides WHERE org_id IS NULL;
ALTER TABLE plan_overrides ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE plan_overrides DROP COLUMN account_id;
ALTER TABLE plan_overrides ADD PRIMARY KEY (org_id);

ALTER TABLE plans DROP COLUMN max_orgs;

DROP INDEX IF EXISTS idx_organizations_account;
ALTER TABLE organizations DROP COLUMN account_id;
DROP TABLE accounts;
