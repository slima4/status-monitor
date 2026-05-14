DROP INDEX IF EXISTS idx_invitations_token_prefix;
ALTER TABLE invitations DROP COLUMN IF EXISTS token_prefix;
