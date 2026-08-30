ALTER TABLE maintenance_windows
    ADD COLUMN suppress_alerts BOOLEAN NOT NULL DEFAULT true;

-- Windows predating the column were announcements; the new default would take
-- their paging dark mid-window.
UPDATE maintenance_windows SET suppress_alerts = false;
