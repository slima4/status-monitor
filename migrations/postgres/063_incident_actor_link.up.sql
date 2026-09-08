-- Acknowledgements that arrive through a notification. Possession is the whole
-- proof, so `actor_id` stays null and the kind carries the meaning.
ALTER TABLE incident_events DROP CONSTRAINT incident_events_actor_type_check;
ALTER TABLE incident_events ADD CONSTRAINT incident_events_actor_type_check
    CHECK (actor_type IN ('system','user','mcp','link'));
