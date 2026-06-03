DROP TABLE IF EXISTS oauth_refresh_tokens;
DROP TABLE IF EXISTS oauth_authorization_codes;
DROP TABLE IF EXISTS oauth_clients;
DROP INDEX IF EXISTS idx_api_tokens_oauth_client;
ALTER TABLE api_tokens DROP COLUMN IF EXISTS oauth_client_id;
ALTER TABLE api_tokens DROP COLUMN IF EXISTS audience;
