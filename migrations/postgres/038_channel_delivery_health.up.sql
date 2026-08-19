-- Delivery health per channel: a dead endpoint otherwise retries for as long
-- as incidents keep opening, with nothing to show for it.
ALTER TABLE notification_channels
    ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0,
    -- Lets the console say how long the endpoint has been dead, not just that.
    ADD COLUMN failing_since TIMESTAMPTZ,
    -- One dead endpoint mails once, however many incidents it swallows.
    ADD COLUMN failing_notified_at TIMESTAMPTZ,
    -- A channel bound only to quiet monitors is never flagged, because nothing
    -- tries to page it. This is what makes that visible.
    ADD COLUMN last_delivered_at TIMESTAMPTZ;

-- Without the backfill every channel reads as never having delivered on the
-- day this ships, including ones that have been fine for a year.
UPDATE notification_channels c
SET last_delivered_at = n.last_sent
FROM (
    SELECT channel_id, max(sent_at) AS last_sent
    FROM incident_notifications
    -- Bounded: this runs at boot while the ALTER above holds the table, and
    -- a channel silent for a quarter reads the same as one with no stamp.
    WHERE status = 'sent'
      AND channel_id IS NOT NULL
      AND sent_at > now() - interval '90 days'
    GROUP BY channel_id
) n
WHERE n.channel_id = c.id;
