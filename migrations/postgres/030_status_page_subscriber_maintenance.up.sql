-- Delivery log for maintenance fan-out: one row per (subscriber, window, phase).
-- Maintenance has no update rows, so the phase ('scheduled' announcement,
-- 'completed' when it ends) is the dedup unit instead of an update id.
CREATE TABLE status_page_subscriber_maintenance (
    id             UUID PRIMARY KEY DEFAULT uuidv7(),
    subscriber_id  UUID NOT NULL REFERENCES status_page_subscribers(id) ON DELETE CASCADE,
    maintenance_id UUID NOT NULL REFERENCES maintenance_windows(id) ON DELETE CASCADE,
    org_id         UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    phase          TEXT NOT NULL CHECK (phase IN ('scheduled', 'completed')),
    status         TEXT NOT NULL CHECK (status IN ('queued', 'sent', 'failed')),
    attempts       INT NOT NULL DEFAULT 0,
    error          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at        TIMESTAMPTZ,
    UNIQUE (subscriber_id, maintenance_id, phase)
);

CREATE INDEX idx_sp_subscriber_maintenance_org
    ON status_page_subscriber_maintenance (org_id, created_at);

CREATE TRIGGER trg_sp_subscriber_maintenance_org_match
    BEFORE INSERT OR UPDATE OF subscriber_id, org_id ON status_page_subscriber_maintenance
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('status_page_subscribers', 'subscriber_id');
