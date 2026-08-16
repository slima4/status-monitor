-- Flap release sweep: triggered incidents whose alert the damper held. Matches
-- due_for_flap_release's leading predicate so the per-tick scan is index-backed
-- instead of reading every triggered row in the database.
CREATE INDEX idx_incidents_flap_release
    ON incidents (started_at)
    WHERE state = 'triggered';

-- The release scan's LATERAL and NOT EXISTS both look up a single incident's
-- notifications; the held row it keys off has no channel.
CREATE INDEX idx_incident_notifications_held
    ON incident_notifications (incident_id, created_at)
    WHERE channel_id IS NULL;
