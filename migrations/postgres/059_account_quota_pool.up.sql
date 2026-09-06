-- The quota subject moves from the org to the account.
--
-- A plan used to sit on `organizations`, so every extra org an owner created
-- carried its own full set of caps: a founding owner with three orgs held
-- 50 + 20 + 20 monitors instead of the 50 the tier promises. Capacity now
-- belongs to the account and its orgs share one pool; an org is a workspace,
-- and a membership grants access, never quota.

CREATE TABLE accounts (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    -- The billing owner. SET NULL rather than CASCADE: purging a user must
    -- not be able to take the account's live orgs with it. Ownerless,
    -- org-less accounts are reaped by the purge job.
    owner_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    plan_id       TEXT NOT NULL DEFAULT 'free' REFERENCES plans(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One account per user. Billing may relax this later; nothing today reads a
-- second account for the same owner, so the constraint keeps the mapping
-- unambiguous.
CREATE UNIQUE INDEX idx_accounts_owner
    ON accounts(owner_user_id) WHERE owner_user_id IS NOT NULL;
CREATE INDEX idx_accounts_plan ON accounts(plan_id);

-- RESTRICT: an account may only be deleted once its orgs are gone, so a
-- cascade can never erase a live tenant through the billing row.
ALTER TABLE organizations
    ADD COLUMN account_id UUID REFERENCES accounts(id) ON DELETE RESTRICT;

-- Owner of record for each org: its earliest owner membership. Orgs with no
-- owner (operator-seeded, or every owner already purged) get an ownerless
-- account of their own, so no org is left without a pool.
CREATE TEMP TABLE mig_org_owner ON COMMIT DROP AS
SELECT o.id AS org_id,
       o.plan_id,
       (SELECT m.user_id FROM memberships m
         WHERE m.org_id = o.id AND m.role = 'owner'
         ORDER BY m.created_at ASC, m.user_id ASC
         LIMIT 1) AS user_id
FROM organizations o;

-- An owner holding orgs on different plans keeps the most generous one:
-- pooling must never silently demote a paid org into a free org's caps.
CREATE TEMP TABLE mig_user_account ON COMMIT DROP AS
SELECT u.user_id,
       uuidv7() AS account_id,
       (SELECT p.id FROM mig_org_owner x JOIN plans p ON p.id = x.plan_id
         WHERE x.user_id = u.user_id
         ORDER BY p.max_targets DESC, p.max_status_pages DESC, p.id ASC
         LIMIT 1) AS plan_id
FROM (SELECT DISTINCT user_id FROM mig_org_owner WHERE user_id IS NOT NULL) u;

CREATE TEMP TABLE mig_orphan_account ON COMMIT DROP AS
SELECT oo.org_id, uuidv7() AS account_id, oo.plan_id
FROM mig_org_owner oo WHERE oo.user_id IS NULL;

INSERT INTO accounts (id, owner_user_id, plan_id)
SELECT account_id, user_id, plan_id FROM mig_user_account;
INSERT INTO accounts (id, owner_user_id, plan_id)
SELECT account_id, NULL, plan_id FROM mig_orphan_account;

UPDATE organizations o
   SET account_id = ua.account_id
  FROM mig_org_owner oo
  JOIN mig_user_account ua ON ua.user_id = oo.user_id
 WHERE o.id = oo.org_id;

UPDATE organizations o
   SET account_id = oa.account_id
  FROM mig_orphan_account oa
 WHERE o.id = oa.org_id;

ALTER TABLE organizations ALTER COLUMN account_id SET NOT NULL;
CREATE INDEX idx_organizations_account ON organizations(account_id);

-- How many orgs one account may hold. Replaces the instance-wide
-- `tenancy.free_tier_owner_org_limit` knob: the number is a tier property,
-- not a deployment property. Existing accounts already over their new limit
-- keep every org — the cap is read at create time only.
ALTER TABLE plans
    ADD COLUMN max_orgs INTEGER NOT NULL DEFAULT 1 CHECK (max_orgs >= 1);
UPDATE plans SET max_orgs = 3 WHERE id = 'founding';
UPDATE plans SET max_orgs = 5 WHERE id = 'pro';

-- Overrides and add-ons follow the plan onto the account: a cap that is
-- pooled cannot be raised per-org without the two disagreeing.
ALTER TABLE plan_overrides
    ADD COLUMN account_id UUID REFERENCES accounts(id) ON DELETE CASCADE;
UPDATE plan_overrides po
   SET account_id = o.account_id
  FROM organizations o
 WHERE o.id = po.org_id;
-- Two orgs of one account can each carry a row, and only one survives the new
-- key. Newest-wins would lose capacity someone granted deliberately: an org
-- holding a raised monitor allowance would be erased by a newer, unrelated
-- page override on a sibling — or by an override that had already expired.
--
-- So the rows are merged instead, per cap, keeping the most generous value any
-- of them granted. Expired rows are dropped first: they grant nothing today,
-- and letting one win on recency would revoke a live allowance. Only numeric
-- values carry over, which is every cap the read path understands (a
-- non-numeric one already makes `QuotaService` ignore the whole row).
CREATE TEMP TABLE mig_override_merge ON COMMIT DROP AS
WITH live AS (
    SELECT account_id, override_json, expires_at, set_by_user_id
    FROM plan_overrides
    WHERE expires_at IS NULL OR expires_at > now()
), caps AS (
    SELECT l.account_id, kv.key, max((kv.value #>> '{}')::numeric) AS value
    FROM live l, jsonb_each(l.override_json) kv
    WHERE jsonb_typeof(kv.value) = 'number'
    GROUP BY l.account_id, kv.key
)
SELECT c.account_id,
       jsonb_object_agg(c.key, to_jsonb(c.value)) AS override_json,
       -- One permanent row makes the merged allowance permanent; otherwise it
       -- runs to the last expiry any of them had.
       (SELECT CASE WHEN bool_or(l.expires_at IS NULL) THEN NULL
                    ELSE max(l.expires_at) END
          FROM live l WHERE l.account_id = c.account_id) AS expires_at,
       (SELECT max(l.set_by_user_id::text)::uuid
          FROM live l WHERE l.account_id = c.account_id) AS set_by_user_id
FROM caps c
GROUP BY c.account_id;

DELETE FROM plan_overrides;
ALTER TABLE plan_overrides DROP COLUMN org_id;
ALTER TABLE plan_overrides ALTER COLUMN account_id SET NOT NULL;
ALTER TABLE plan_overrides ADD PRIMARY KEY (account_id);

INSERT INTO plan_overrides (account_id, override_json, reason, set_by_user_id, expires_at)
SELECT account_id, override_json,
       'merged from the org-scoped overrides when quotas moved to the account',
       set_by_user_id, expires_at
FROM mig_override_merge;

-- Add-ons are additive capacity, so the account's rows sum rather than the
-- newest winning. `created_at` / `updated_at` reset here; billing owns the
-- table and writes no rows yet.
ALTER TABLE org_addons
    ADD COLUMN account_id UUID REFERENCES accounts(id) ON DELETE CASCADE;
UPDATE org_addons a
   SET account_id = o.account_id
  FROM organizations o
 WHERE o.id = a.org_id;
CREATE TEMP TABLE mig_addons ON COMMIT DROP AS
SELECT account_id, addon_type, sum(quantity)::int AS quantity
FROM org_addons GROUP BY account_id, addon_type;
DELETE FROM org_addons;
ALTER TABLE org_addons DROP COLUMN org_id;
ALTER TABLE org_addons ALTER COLUMN account_id SET NOT NULL;
ALTER TABLE org_addons ADD PRIMARY KEY (account_id, addon_type);
INSERT INTO org_addons (account_id, addon_type, quantity)
SELECT account_id, addon_type, quantity FROM mig_addons;
ALTER TABLE org_addons RENAME TO account_addons;

-- Nobody is retro-actively over the new org cap. An account already holding
-- more orgs than its plan allows keeps them through an override sized to what
-- it has, tombstones included: the cap is read at create time *and* at restore,
-- so without this a grandfathered account could never recover an org it deleted
-- inside the grace window. On the hosted service this selects nothing today.
INSERT INTO plan_overrides (account_id, override_json, reason)
SELECT a.id,
       jsonb_build_object('max_orgs', c.n),
       'grandfathered when quotas moved to the account'
FROM accounts a
JOIN plans p ON p.id = a.plan_id
JOIN LATERAL (
    SELECT count(*) AS n FROM organizations o WHERE o.account_id = a.id
) c ON true
WHERE c.n > p.max_orgs
ON CONFLICT (account_id) DO UPDATE
    SET override_json = plan_overrides.override_json || EXCLUDED.override_json;

-- The plan now has exactly one home.
ALTER TABLE organizations DROP COLUMN plan_id;
