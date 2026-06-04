-- Escalation policies: the ordered ladder of who to page and how long to wait
-- before paging the next rung. A monitor (or the org default) points at one
-- policy; the escalation engine walks its steps, re-paging until the incident
-- is acknowledged or the ladder is exhausted.
--
-- Migration order: on-call schedules (a future migration) are numbered higher
-- and add their own tables + the escalation_targets.schedule_id foreign key via
-- ALTER. Until then escalation_targets.schedule_id is an unconstrained column
-- and only 'channel' targets route to a real delivery; the engine skips
-- 'user'/'schedule' targets it cannot yet resolve.

CREATE TABLE escalation_policies (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    description  TEXT,
    -- Re-walk the whole ladder this many extra times if still unacknowledged.
    repeat_count INTEGER NOT NULL DEFAULT 0 CHECK (repeat_count >= 0),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at   TIMESTAMPTZ
);
CREATE UNIQUE INDEX idx_escalation_policies_name
    ON escalation_policies (org_id, name) WHERE deleted_at IS NULL;
CREATE INDEX idx_escalation_policies_org
    ON escalation_policies (org_id) WHERE deleted_at IS NULL;

-- One rung of the ladder. The engine waits `delay_secs` after paging this
-- level before advancing to the next.
CREATE TABLE escalation_steps (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    policy_id   UUID NOT NULL REFERENCES escalation_policies(id) ON DELETE CASCADE,
    level       INTEGER NOT NULL CHECK (level >= 1),
    delay_secs  INTEGER NOT NULL DEFAULT 300 CHECK (delay_secs >= 0),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_escalation_steps_level ON escalation_steps (policy_id, level);
CREATE INDEX idx_escalation_steps_policy ON escalation_steps (org_id, policy_id, level);

-- Who a step pages. Exactly one of user/schedule/channel is set, matching
-- target_type. schedule_id stays FK-less until the on-call migration exists.
CREATE TABLE escalation_targets (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    step_id      UUID NOT NULL REFERENCES escalation_steps(id) ON DELETE CASCADE,
    target_type  TEXT NOT NULL CHECK (target_type IN ('user','schedule','channel')),
    user_id      UUID REFERENCES users(id) ON DELETE CASCADE,
    schedule_id  UUID,
    channel_id   UUID REFERENCES notification_channels(id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(user_id, schedule_id, channel_id) = 1)
);
CREATE INDEX idx_escalation_targets_step ON escalation_targets (org_id, step_id);
-- Cover the cascading FKs so deleting a channel/user does not seq-scan targets.
CREATE INDEX idx_escalation_targets_channel
    ON escalation_targets (channel_id) WHERE channel_id IS NOT NULL;
CREATE INDEX idx_escalation_targets_user
    ON escalation_targets (user_id) WHERE user_id IS NOT NULL;

-- Org-match triggers down the parent chain (mirror 006).
CREATE TRIGGER trg_escalation_steps_org_match
    BEFORE INSERT OR UPDATE OF policy_id, org_id ON escalation_steps
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('escalation_policies', 'policy_id');
CREATE TRIGGER trg_escalation_targets_org_match
    BEFORE INSERT OR UPDATE OF step_id, org_id ON escalation_targets
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('escalation_steps', 'step_id');

-- Org default policy: resolution order at trigger time is
-- target.escalation_policy_id → org.default_escalation_policy_id → none.
ALTER TABLE organizations
    ADD COLUMN default_escalation_policy_id UUID
        REFERENCES escalation_policies(id) ON DELETE SET NULL;

-- Wire the foreign keys 018 left deferred (the table now exists).
ALTER TABLE incidents
    ADD CONSTRAINT fk_incidents_escalation_policy
    FOREIGN KEY (escalation_policy_id) REFERENCES escalation_policies(id) ON DELETE SET NULL;
ALTER TABLE targets
    ADD CONSTRAINT fk_targets_escalation_policy
    FOREIGN KEY (escalation_policy_id) REFERENCES escalation_policies(id) ON DELETE SET NULL;
CREATE INDEX idx_targets_escalation_policy
    ON targets (escalation_policy_id) WHERE escalation_policy_id IS NOT NULL;

-- Quota cap. Generous default so the free tier is not gated today; the Pro
-- tier carries a higher allowance. Exact tier values track the plan-tiers
-- decision separately.
ALTER TABLE plans
    ADD COLUMN max_escalation_policies INTEGER NOT NULL DEFAULT 10;
UPDATE plans SET max_escalation_policies = 50 WHERE id = 'pro';
