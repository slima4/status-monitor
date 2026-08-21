-- Both constraints keep their generated names; the drift test looks them up by name.
ALTER TABLE oauth_identities
    DROP CONSTRAINT oauth_identities_provider_check,
    ADD CONSTRAINT oauth_identities_provider_check
        CHECK (provider IN ('github', 'google', 'microsoft'));

ALTER TABLE oauth_states
    DROP CONSTRAINT oauth_states_provider_check,
    ADD CONSTRAINT oauth_states_provider_check
        CHECK (provider IN ('github', 'google', 'microsoft', 'slack_connect', 'discord_connect'));
