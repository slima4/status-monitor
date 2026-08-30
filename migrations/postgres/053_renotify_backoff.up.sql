-- Constraint keeps its generated name; the drift test looks it up by name.
ALTER TABLE incident_notifications
    DROP CONSTRAINT incident_notifications_reason_check,
    ADD CONSTRAINT incident_notifications_reason_check
        CHECK (reason IN ('opened','escalated','resolved','reopened','no_data','data_resumed','reminder'));

-- Reset by reopen, so a reopened incident pages at the base interval again.
ALTER TABLE incidents
    ADD COLUMN renotify_count INTEGER NOT NULL DEFAULT 0 CHECK (renotify_count >= 0);
