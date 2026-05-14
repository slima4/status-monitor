DROP TRIGGER IF EXISTS trg_maintenance_components_org_match ON maintenance_window_components;
DROP TRIGGER IF EXISTS trg_incident_updates_org_match ON incident_updates;
DROP FUNCTION IF EXISTS assert_org_matches_parent();
