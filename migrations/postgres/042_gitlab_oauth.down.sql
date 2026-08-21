-- A GitLab-only user loses their one credential here and gets back in only
-- through a magic link.
DELETE FROM oauth_identities WHERE provider = 'gitlab';
DELETE FROM oauth_states WHERE provider = 'gitlab';

ALTER TABLE oauth_identities
    DROP CONSTRAINT oauth_identities_provider_check,
    ADD CONSTRAINT oauth_identities_provider_check
        CHECK (provider IN ('github', 'google', 'microsoft'));

ALTER TABLE oauth_states
    DROP CONSTRAINT oauth_states_provider_check,
    ADD CONSTRAINT oauth_states_provider_check
        CHECK (provider IN ('github', 'google', 'microsoft', 'slack_connect', 'discord_connect'));
