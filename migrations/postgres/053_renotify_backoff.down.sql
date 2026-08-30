ALTER TABLE incidents DROP COLUMN IF EXISTS renotify_count;

-- The narrowed constraint would reject these rows.
UPDATE incident_notifications SET reason = 'opened' WHERE reason = 'reminder';

ALTER TABLE incident_notifications
    DROP CONSTRAINT incident_notifications_reason_check,
    ADD CONSTRAINT incident_notifications_reason_check
        CHECK (reason IN ('opened','escalated','resolved','reopened','no_data','data_resumed'));
