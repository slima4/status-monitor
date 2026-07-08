-- Per-plan cap on browser-flow monitors (heavy: one browser per check). 0 = off,
-- which doubles as the gate; every plan stays 0 until launch sets real caps.
ALTER TABLE plans
    ADD COLUMN max_flow_checks INTEGER NOT NULL DEFAULT 0;
