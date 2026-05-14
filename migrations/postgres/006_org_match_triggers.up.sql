-- Belt-and-suspenders guard: refuse child rows whose org_id does not match the
-- parent's. Catches application bugs that would otherwise write cross-org rows
-- past the type-enforced repository layer.

CREATE OR REPLACE FUNCTION assert_org_matches_parent() RETURNS TRIGGER AS $$
DECLARE
    parent_org UUID;
BEGIN
    IF TG_TABLE_NAME = 'incident_updates' THEN
        SELECT org_id INTO parent_org FROM incidents WHERE id = NEW.incident_id;
    ELSIF TG_TABLE_NAME = 'maintenance_window_components' THEN
        SELECT org_id INTO parent_org FROM maintenance_windows WHERE id = NEW.maintenance_id;
    END IF;

    IF parent_org IS NULL OR parent_org <> NEW.org_id THEN
        RAISE EXCEPTION 'org_id mismatch: child=% parent=%', NEW.org_id, parent_org;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog, public;

-- UPDATE coverage closes a future hole where reparenting a child row to a
-- different incident/maintenance window would dodge the trigger.
CREATE TRIGGER trg_incident_updates_org_match
    BEFORE INSERT OR UPDATE OF incident_id, org_id ON incident_updates
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent();

CREATE TRIGGER trg_maintenance_components_org_match
    BEFORE INSERT OR UPDATE OF maintenance_id, org_id ON maintenance_window_components
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent();
