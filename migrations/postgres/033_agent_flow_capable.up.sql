-- Whether an agent can run browser-flow monitors. Self-reported on config pull;
-- routing sends flow checks only to capable agents.
ALTER TABLE agents ADD COLUMN flow_capable BOOLEAN NOT NULL DEFAULT false;
