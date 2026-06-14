-- A monitor is "silent" when every region it is assigned to has lost its probe
-- (no fresh, enabled agent) — detected by the silence sweep from agent liveness,
-- not from check results (a failing check still reports an Error). This table
-- only records the open/resolved boundary so a silence is announced once and a
-- recovery once. Healthy monitors have no row, so it stays tiny.
CREATE TABLE monitor_silence_state (
    target_id    UUID PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    silent_since TIMESTAMPTZ NOT NULL,
    notified_at  TIMESTAMPTZ,
    resolved_at  TIMESTAMPTZ
);

-- Row's org_id must match the target's, blocking a cross-tenant write.
CREATE TRIGGER trg_monitor_silence_state_target_org
    BEFORE INSERT OR UPDATE OF target_id, org_id ON monitor_silence_state
    FOR EACH ROW EXECUTE FUNCTION assert_target_in_same_org();
