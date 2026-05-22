-- Belt-and-suspenders guard: refuse child rows whose org_id does not match
-- the parent's. Catches application bugs that would otherwise write
-- cross-org rows past the type-enforced repository layer.
--
-- The parent table and FK column travel with the trigger declaration via
-- `TG_ARGV`, so the function stays generic and adding a new tenant child
-- table is a single `CREATE TRIGGER ... EXECUTE FUNCTION
-- assert_org_matches_parent('<parent>', '<fk_col>')` — no branch to extend
-- in this function, no chance of a missing `TG_TABLE_NAME` case silently
-- letting cross-org rows through.
CREATE OR REPLACE FUNCTION assert_org_matches_parent() RETURNS TRIGGER AS $$
DECLARE
    parent_table  TEXT := TG_ARGV[0];
    parent_fk_col TEXT := TG_ARGV[1];
    fk_value      UUID;
    parent_org    UUID;
BEGIN
    -- Access NEW's FK column dynamically via row-to-jsonb so the function
    -- doesn't have to know the field's name at compile time.
    fk_value := (to_jsonb(NEW) ->> parent_fk_col)::uuid;
    EXECUTE format('SELECT org_id FROM %I WHERE id = $1', parent_table)
        INTO parent_org USING fk_value;
    IF parent_org IS NULL OR parent_org <> NEW.org_id THEN
        RAISE EXCEPTION 'org_id mismatch on %.%: child_org=% parent_table=% parent_id=% parent_org=%',
            TG_TABLE_NAME, parent_fk_col, NEW.org_id, parent_table, fk_value, parent_org;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog, public;

-- Second-axis guard for tables that also carry a denormalised target_id:
-- ensures the target lives in the same org as the row being inserted, not
-- just that the row's `maintenance_id` parent does. Today only
-- `maintenance_window_components` needs it; a future tenant child table
-- with a `target_id` denormalisation should declare its own trigger.
CREATE OR REPLACE FUNCTION assert_target_in_same_org() RETURNS TRIGGER AS $$
DECLARE
    target_org UUID;
BEGIN
    SELECT org_id INTO target_org FROM targets WHERE id = NEW.target_id;
    IF target_org IS NULL OR target_org <> NEW.org_id THEN
        RAISE EXCEPTION 'target_id % belongs to org % but row inserted under org %',
            NEW.target_id, target_org, NEW.org_id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql
SET search_path = pg_catalog, public;

-- UPDATE coverage closes a future hole where reparenting a child row to a
-- different incident/maintenance window or re-pointing it at another
-- target would otherwise dodge the trigger.
CREATE TRIGGER trg_incident_updates_org_match
    BEFORE INSERT OR UPDATE OF incident_id, org_id ON incident_updates
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('incidents', 'incident_id');

CREATE TRIGGER trg_maintenance_components_org_match
    BEFORE INSERT OR UPDATE OF maintenance_id, org_id, target_id ON maintenance_window_components
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('maintenance_windows', 'maintenance_id');

CREATE TRIGGER trg_maintenance_components_target_org
    BEFORE INSERT OR UPDATE OF target_id, org_id ON maintenance_window_components
    FOR EACH ROW EXECUTE FUNCTION assert_target_in_same_org();
