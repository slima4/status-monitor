-- Inbound heartbeat state, one row per heartbeat-kind target. The check
-- evaluates the anchor age GREATEST(armed_at, last_ping_at) against
-- period+grace. last_ping_at is real pings only ("last ping" in UI/API);
-- armed_at is the re-arm point (creation + every disabled→enabled flip) so a
-- pause doesn't open an incident on resume without fabricating a ping.
-- Token discipline mirrors monitor_shares: token_hash (SHA-256 hex) is the
-- lookup key, token_enc a KEK-encrypted copy for re-copying the ping URL.
CREATE TABLE IF NOT EXISTS heartbeat_monitors (
    target_id    UUID PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    token_hash   TEXT NOT NULL UNIQUE,
    token_enc    TEXT NOT NULL,
    last_ping_at TIMESTAMPTZ,
    armed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- org_id must match the target's, blocking a cross-tenant write via a
-- request-supplied target_id.
CREATE TRIGGER trg_heartbeat_monitors_target_org
    BEFORE INSERT OR UPDATE OF target_id, org_id ON heartbeat_monitors
    FOR EACH ROW EXECUTE FUNCTION assert_target_in_same_org();

-- The scheduler's enabled-heartbeat enumeration and the per-refresh heal (which
-- must also see disabled rows) both filter targets by kind; without this they
-- scan every target for a small subset.
CREATE INDEX IF NOT EXISTS idx_targets_heartbeat
    ON targets(id) WHERE kind = 'heartbeat';
