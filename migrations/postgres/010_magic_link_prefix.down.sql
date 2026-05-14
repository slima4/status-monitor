DROP INDEX IF EXISTS idx_magic_link_tokens_prefix;
ALTER TABLE magic_link_tokens DROP COLUMN IF EXISTS token_prefix;
