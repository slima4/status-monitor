-- Delivery log for public-subscriber fan-out: one row per (subscriber, public
-- incident update) the dispatcher has claimed. The UNIQUE pair makes the sweep
-- idempotent and lets a row be claimed exactly once across overlapping ticks or
-- replicas — insert-on-conflict-do-nothing is the claim.
CREATE TABLE status_page_subscriber_notifications (
    id                 UUID PRIMARY KEY DEFAULT uuidv7(),
    subscriber_id      UUID NOT NULL REFERENCES status_page_subscribers(id) ON DELETE CASCADE,
    incident_update_id UUID NOT NULL REFERENCES incident_updates(id) ON DELETE CASCADE,
    org_id             UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    status             TEXT NOT NULL CHECK (status IN ('queued', 'sent', 'failed')),
    attempts           INT NOT NULL DEFAULT 0,
    error              TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at            TIMESTAMPTZ,
    UNIQUE (subscriber_id, incident_update_id)
);

CREATE INDEX idx_status_page_subscriber_notifications_org
    ON status_page_subscriber_notifications (org_id, created_at);

CREATE TRIGGER trg_status_page_subscriber_notifications_org_match
    BEFORE INSERT OR UPDATE OF subscriber_id, org_id ON status_page_subscriber_notifications
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('status_page_subscribers', 'subscriber_id');
