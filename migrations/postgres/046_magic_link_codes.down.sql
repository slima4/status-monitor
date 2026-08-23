DROP INDEX IF EXISTS idx_magic_link_tokens_sent;
DROP INDEX IF EXISTS idx_magic_link_tokens_nonce;

ALTER TABLE magic_link_tokens
    DROP COLUMN IF EXISTS code_hash,
    DROP COLUMN IF EXISTS code_spent_at,
    DROP COLUMN IF EXISTS nonce_hash,
    DROP COLUMN IF EXISTS sent_at,
    DROP COLUMN IF EXISTS superseded_at,
    DROP COLUMN IF EXISTS redeemed_via;
