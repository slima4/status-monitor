ALTER TABLE incidents
    ADD COLUMN counts_as_downtime BOOLEAN NOT NULL DEFAULT true;

ALTER TABLE incident_events DROP CONSTRAINT incident_events_kind_check;
ALTER TABLE incident_events ADD CONSTRAINT incident_events_kind_check
    CHECK (kind IN (
      'triggered','acknowledged','assigned','unassigned',
      'escalated','notified','note','severity_changed','downtime_changed',
      'state_changed','resolved','reopened','published','unpublished',
      'postmortem_published','postmortem_unpublished'
    ));

UPDATE incidents SET counts_as_downtime = false WHERE origin = 'manual';

INSERT INTO incident_events (org_id, incident_id, kind, actor_type, message)
SELECT org_id, id, 'downtime_changed', 'system',
       'declared incidents no longer count toward uptime by default'
FROM incidents
WHERE origin = 'manual';
