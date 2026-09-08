UPDATE incident_events SET actor_type = 'system' WHERE actor_type = 'link';
ALTER TABLE incident_events DROP CONSTRAINT incident_events_actor_type_check;
ALTER TABLE incident_events ADD CONSTRAINT incident_events_actor_type_check
    CHECK (actor_type IN ('system','user','mcp'));
