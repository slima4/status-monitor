-- Single-use account-recovery tokens for the soft-delete / undo flow.
--
-- A user who deletes their account is soft-deleted (users.deleted_at) and
-- handed a recovery link. The raw token is never stored: `token_prefix` is
-- the indexed lookup key and `token_hash` is the argon2id hash the redeem
-- path verifies against — the same prefix-narrow-then-argon2 pattern as
-- api_tokens / invitations. The FK is ON DELETE CASCADE so the eventual
-- hard-purge of the user row erases the token alongside everything else.
CREATE TABLE user_recovery_tokens (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash      TEXT NOT NULL,
    token_prefix    TEXT NOT NULL UNIQUE,
    expires_at      TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    used_at         TIMESTAMPTZ
);

-- Active-token lookup by user (the redeem path also narrows by prefix via
-- the UNIQUE constraint above).
CREATE INDEX idx_user_recovery_tokens_active
    ON user_recovery_tokens(user_id, expires_at)
    WHERE used_at IS NULL;

-- At most one active (unused) recovery token per user. A second deletion
-- attempt on an already-soft-deleted user hits this and the handler maps
-- the unique-violation to 422 instead of minting a second valid token.
CREATE UNIQUE INDEX idx_user_recovery_tokens_one_active_per_user
    ON user_recovery_tokens(user_id)
    WHERE used_at IS NULL;
