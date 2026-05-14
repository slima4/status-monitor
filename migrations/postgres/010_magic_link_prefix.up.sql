-- Phase 8 magic-link prefix narrowing.
--
-- Without `token_prefix`, `magic_link::consume` had to argon2-verify every
-- unused-non-expired row. An attacker who can pump request volume (even rate-
-- limited per IP via residential proxies) can fill the table with live rows
-- and amplify CPU per verify request to seconds. Adding a 16-char prefix lets
-- the lookup narrow to ~1 row before argon2 runs.
--
-- Mirrors the same pattern in `invitations.token_prefix` (009).

ALTER TABLE magic_link_tokens ADD COLUMN token_prefix TEXT NOT NULL DEFAULT '';

CREATE INDEX idx_magic_link_tokens_prefix
    ON magic_link_tokens(token_prefix)
    WHERE used_at IS NULL;
