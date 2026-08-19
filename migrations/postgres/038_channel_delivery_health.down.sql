ALTER TABLE notification_channels
    DROP COLUMN consecutive_failures,
    DROP COLUMN failing_since,
    DROP COLUMN failing_notified_at,
    DROP COLUMN last_delivered_at;
