-- Narrow invitation lookup from a table-scan to an indexed prefix probe.
--
-- The Phase 5 redeem path used to scan every pending row and argon2-verify
-- each candidate — at the default per-org cap of 50, that's `50 * orgs`
-- verifies per accept. argon2id at default cost is ~50ms; the linear scan
-- becomes a CPU-amplification DoS at any non-trivial pending count.
--
-- Storing the first 16 base64url chars of the raw token (96 bits of entropy)
-- gives the same shape as `api_tokens.token_prefix`: indexed for narrowing,
-- argon2 verify disambiguates. Collisions are vanishingly rare; not UNIQUE
-- for the same reason as api_tokens (rare prefix collision must not 500).

ALTER TABLE invitations
    ADD COLUMN token_prefix TEXT NOT NULL DEFAULT '';

CREATE INDEX idx_invitations_token_prefix ON invitations(token_prefix);
