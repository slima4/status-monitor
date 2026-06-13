-- On-call schedules: who is reachable right now. A schedule is a stack of
-- rotation layers; higher layer_order wins. Who-is-on-call is computed at page
-- time from these rows (a pure resolver), never materialised — no cron job
-- writes shift rows. A one-off override beats the computed rotation for its
-- window.
--
-- This migration also wires the escalation_targets.schedule_id foreign key that
-- 020 left unconstrained (the schedules table now exists), and adds the
-- per-user contact channels that a `user`/`schedule` escalation target pages
-- through.

CREATE TABLE on_call_schedules (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    timezone    TEXT NOT NULL DEFAULT 'UTC',   -- IANA tz; rotation boundaries computed here
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ
);
CREATE UNIQUE INDEX idx_on_call_schedules_name
    ON on_call_schedules (org_id, name) WHERE deleted_at IS NULL;
CREATE INDEX idx_on_call_schedules_org
    ON on_call_schedules (org_id) WHERE deleted_at IS NULL;

-- One rotation within a schedule. Participants rotate through their positions
-- every rotation_length_secs, anchored at handoff_at.
CREATE TABLE on_call_layers (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id               UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    schedule_id          UUID NOT NULL REFERENCES on_call_schedules(id) ON DELETE CASCADE,
    name                 TEXT,
    rotation_type        TEXT NOT NULL CHECK (rotation_type IN ('daily','weekly','custom')),
    rotation_length_secs INTEGER NOT NULL CHECK (rotation_length_secs > 0),
    handoff_at           TIMESTAMPTZ NOT NULL,
    layer_order          INTEGER NOT NULL DEFAULT 0,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_on_call_layers_schedule ON on_call_layers (org_id, schedule_id, layer_order);

-- Ordered participants in a layer; the rotation cycles through them by position.
CREATE TABLE on_call_participants (
    id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id    UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    layer_id  UUID NOT NULL REFERENCES on_call_layers(id) ON DELETE CASCADE,
    user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL CHECK (position >= 0)
);
CREATE UNIQUE INDEX idx_on_call_participants_pos ON on_call_participants (layer_id, position);
-- Cover the cascading user FK so deleting a user does not seq-scan participants.
CREATE INDEX idx_on_call_participants_user ON on_call_participants (user_id);

-- One-off coverage swaps; an override beats the computed rotation in its window.
CREATE TABLE on_call_overrides (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    schedule_id UUID NOT NULL REFERENCES on_call_schedules(id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    starts_at   TIMESTAMPTZ NOT NULL,
    ends_at     TIMESTAMPTZ NOT NULL,
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX idx_on_call_overrides_window
    ON on_call_overrides (org_id, schedule_id, starts_at, ends_at);
-- Cover the cascading user FK so deleting a user does not seq-scan overrides.
CREATE INDEX idx_on_call_overrides_user ON on_call_overrides (user_id);

-- Org-match triggers down the parent chain (mirror 006).
CREATE TRIGGER trg_on_call_layers_org_match
    BEFORE INSERT OR UPDATE OF schedule_id, org_id ON on_call_layers
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('on_call_schedules', 'schedule_id');
CREATE TRIGGER trg_on_call_participants_org_match
    BEFORE INSERT OR UPDATE OF layer_id, org_id ON on_call_participants
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('on_call_layers', 'layer_id');
CREATE TRIGGER trg_on_call_overrides_org_match
    BEFORE INSERT OR UPDATE OF schedule_id, org_id ON on_call_overrides
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('on_call_schedules', 'schedule_id');

-- The schedules table now exists, so constrain the escalation target FK 020 left open.
ALTER TABLE escalation_targets
    ADD CONSTRAINT fk_escalation_targets_schedule
    FOREIGN KEY (schedule_id) REFERENCES on_call_schedules(id) ON DELETE CASCADE;
-- Cover the cascade (019 indexed channel_id/user_id for the same reason but
-- schedule_id arrived here in 020): deleting a schedule must not seq-scan.
CREATE INDEX idx_escalation_targets_schedule
    ON escalation_targets (schedule_id) WHERE schedule_id IS NOT NULL;

-- Second-axis org-match backstop for escalation_targets: the step_id chain is
-- covered by 020's trigger, but the denormalised user/schedule/channel ids have
-- only an app-layer guard. Validate the set id's org matches the row's org so a
-- raw write can never bind one org's rung to another org's responder/channel
-- (mirror 006's assert_target_in_same_org). Created here because it references
-- on_call_schedules, which exists only now.
CREATE OR REPLACE FUNCTION assert_escalation_target_refs_org() RETURNS TRIGGER AS $$
DECLARE
    ref_org UUID;
BEGIN
    IF NEW.user_id IS NOT NULL THEN
        PERFORM 1 FROM memberships WHERE user_id = NEW.user_id AND org_id = NEW.org_id;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'escalation_target user_id % is not a member of org %',
                NEW.user_id, NEW.org_id;
        END IF;
    END IF;
    IF NEW.schedule_id IS NOT NULL THEN
        SELECT org_id INTO ref_org FROM on_call_schedules WHERE id = NEW.schedule_id;
        IF ref_org IS NULL OR ref_org <> NEW.org_id THEN
            RAISE EXCEPTION 'escalation_target schedule_id % is not in org %',
                NEW.schedule_id, NEW.org_id;
        END IF;
    END IF;
    IF NEW.channel_id IS NOT NULL THEN
        SELECT org_id INTO ref_org FROM notification_channels WHERE id = NEW.channel_id;
        IF ref_org IS NULL OR ref_org <> NEW.org_id THEN
            RAISE EXCEPTION 'escalation_target channel_id % is not in org %',
                NEW.channel_id, NEW.org_id;
        END IF;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql SET search_path = pg_catalog, public;

CREATE TRIGGER trg_escalation_targets_refs_org
    BEFORE INSERT OR UPDATE OF user_id, schedule_id, channel_id, org_id ON escalation_targets
    FOR EACH ROW EXECUTE FUNCTION assert_escalation_target_refs_org();

-- Per-user contact channels: which org notification channels page a given
-- member when a user/schedule escalation target resolves to them.
CREATE TABLE user_contact_channels (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id UUID NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, user_id, channel_id)
);
CREATE INDEX idx_user_contact_channels_user ON user_contact_channels (org_id, user_id);
-- Cover the cascading channel FK so deleting a channel does not seq-scan these.
CREATE INDEX idx_user_contact_channels_channel ON user_contact_channels (channel_id);
CREATE TRIGGER trg_user_contact_channels_org_match
    BEFORE INSERT OR UPDATE OF channel_id, org_id ON user_contact_channels
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('notification_channels', 'channel_id');

-- Quota cap + gate. Generous default so the free tier is not gated today; the
-- Pro tier carries a higher allowance. Exact tier values track the plan-tiers
-- decision separately.
ALTER TABLE plans
    ADD COLUMN max_on_call_schedules INTEGER NOT NULL DEFAULT 5,
    ADD COLUMN on_call_enabled       BOOLEAN NOT NULL DEFAULT true;
UPDATE plans SET max_on_call_schedules = 25 WHERE id = 'pro';
