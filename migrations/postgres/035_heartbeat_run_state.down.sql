ALTER TABLE heartbeat_monitors
    DROP COLUMN last_start_at,
    DROP COLUMN last_fail_at,
    DROP COLUMN last_exit_code;
