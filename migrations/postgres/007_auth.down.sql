DROP TABLE IF EXISTS login_attempts;
DROP TABLE IF EXISTS magic_link_tokens;
DROP TABLE IF EXISTS invitations;
DROP TABLE IF EXISTS api_tokens;
DROP TABLE IF EXISTS oauth_states;
DROP TABLE IF EXISTS oauth_identities;
DROP TABLE IF EXISTS sessions;

ALTER TABLE users
    DROP COLUMN IF EXISTS last_seen_at,
    DROP COLUMN IF EXISTS email_verified_at;
