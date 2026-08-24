-- A heartbeat with no ping has no health: not up, not down, no data. Until
-- this is set the monitor is not dispatched, so it emits nothing and pages
-- nobody. Set by the first ping of any signal, never cleared; independent of
-- armed_at, because a resume re-arms but does not unwire.
ALTER TABLE heartbeat_monitors
    ADD COLUMN IF NOT EXISTS first_ping_at TIMESTAMPTZ;

-- Anything that has ever pinged keeps alerting exactly as it does today.
UPDATE heartbeat_monitors
   SET first_ping_at = LEAST(
           COALESCE(last_ping_at,  'infinity'),
           COALESCE(last_start_at, 'infinity'),
           COALESCE(last_fail_at,  'infinity'))
 WHERE first_ping_at IS NULL
   AND COALESCE(last_ping_at, last_start_at, last_fail_at) IS NOT NULL;

-- An unwired monitor emits nothing from here on, and an incident clears only
-- on a sustained run of genuine Up, so one open now would hang open forever.
-- Silent by design: this retracts a false alarm rather than reporting a fix,
-- and the writer notifies at write time so it never re-reads these rows.
WITH unwired AS (
    SELECT target_id FROM heartbeat_monitors WHERE first_ping_at IS NULL
), closed AS (
    UPDATE incidents i
       SET ended_at          = now(),
           duration_secs     = GREATEST(0, EXTRACT(EPOCH FROM (now() - i.started_at))::int),
           state             = 'resolved',
           resolved_by       = NULL,
           next_escalation_at = NULL,
           updated_at        = now()
      FROM unwired u
     WHERE i.target_id = u.target_id
       AND i.ended_at IS NULL
       AND i.origin = 'monitor'
 RETURNING i.id, i.org_id, i.visibility
)
INSERT INTO incident_updates (org_id, incident_id, phase, message, author)
SELECT org_id, id, 'resolved',
       'Withdrawn: this heartbeat had never received a ping, so it was never down.',
       'system'
  FROM closed
 WHERE visibility = 'public';
