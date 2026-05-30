-- Per-user 12h/24h preference for the client-side timestamp renderer
-- (localtime.js). 'auto' keeps the browser-locale default, i.e. the existing
-- behaviour for every current row.
ALTER TABLE users
    ADD COLUMN time_format TEXT NOT NULL DEFAULT 'auto'
    CHECK (time_format IN ('auto', '12h', '24h'));
