-- Retrospective document for a resolved incident: one per incident, authored
-- and published independently of the public narration timeline.
CREATE TABLE incident_postmortems (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    incident_id  UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    summary      TEXT,
    root_cause   TEXT,
    impact       TEXT,
    action_items JSONB NOT NULL DEFAULT '[]'::jsonb,   -- [{text, owner_user_id, done}]
    author_id    UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX idx_incident_postmortems_incident ON incident_postmortems (incident_id);

CREATE TRIGGER trg_incident_postmortems_org_match
    BEFORE INSERT OR UPDATE OF incident_id, org_id ON incident_postmortems
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('incidents', 'incident_id');
