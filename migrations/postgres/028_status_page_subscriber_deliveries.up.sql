-- Unified delivery log for public-subscriber fan-out, across event types
-- (`incident_update` and `maintenance`). One row per (subscriber, event, phase);
-- the UNIQUE makes the sweep idempotent and lets a row be claimed exactly once
-- across overlapping ticks or replicas — insert-on-conflict-do-nothing is the
-- claim. `event_ref` is the source id (an incident_update or a maintenance
-- window); it carries no FK because it points at two tables, so orphaned rows
-- after a source delete are reaped by the retention purge instead of a cascade.
-- `phase` distinguishes a window's 'scheduled'/'completed' notices; incident
-- updates use the empty default so the UNIQUE stays meaningful (NULLs wouldn't).
CREATE TABLE status_page_subscriber_deliveries (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    subscriber_id UUID NOT NULL REFERENCES status_page_subscribers(id) ON DELETE CASCADE,
    org_id        UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    event_kind    TEXT NOT NULL CHECK (event_kind IN ('incident_update', 'maintenance')),
    event_ref     UUID NOT NULL,
    phase         TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL CHECK (status IN ('queued', 'sent', 'failed')),
    attempts      INT NOT NULL DEFAULT 0,
    error         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at       TIMESTAMPTZ,
    UNIQUE (subscriber_id, event_kind, event_ref, phase)
);

CREATE INDEX idx_sp_subscriber_deliveries_org
    ON status_page_subscriber_deliveries (org_id, created_at);

CREATE TRIGGER trg_sp_subscriber_deliveries_org_match
    BEFORE INSERT OR UPDATE OF subscriber_id, org_id ON status_page_subscriber_deliveries
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('status_page_subscribers', 'subscriber_id');
