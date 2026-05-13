DROP INDEX IF EXISTS idx_maintenance_window_components_target;
DROP TABLE IF EXISTS maintenance_window_components;

DROP INDEX IF EXISTS idx_maintenance_window_range;
DROP TABLE IF EXISTS maintenance_windows;

DROP INDEX IF EXISTS idx_incident_updates_incident;
DROP TABLE IF EXISTS incident_updates;

DROP INDEX IF EXISTS idx_incidents_started;
DROP INDEX IF EXISTS idx_incidents_open;
DROP INDEX IF EXISTS idx_incidents_target_started;
DROP TABLE IF EXISTS incidents;
