-- Only what evaluation needs; per-ping history lives in ClickHouse heartbeat_pings.
ALTER TABLE heartbeat_monitors
    -- A run is open while this is later than both finishes below.
    ADD COLUMN last_start_at  TIMESTAMPTZ,
    -- Down until a success is newer, so a re-arm clears it via armed_at too.
    ADD COLUMN last_fail_at   TIMESTAMPTZ,
    -- NULL when the signal was the bare word rather than an exit status.
    ADD COLUMN last_exit_code SMALLINT;
