-- A code beside the link in the same mail, for the reader whose inbox is on
-- another device.
ALTER TABLE magic_link_tokens
    ADD COLUMN code_hash     TEXT,
    ADD COLUMN code_spent_at TIMESTAMPTZ,
    ADD COLUMN nonce_hash    TEXT,
    -- Only a row whose mail actually went out is redeemable or counts against
    -- the throttle; every request inserts one regardless.
    ADD COLUMN sent_at       TIMESTAMPTZ,
    ADD COLUMN superseded_at TIMESTAMPTZ,
    ADD COLUMN redeemed_via  TEXT
        CHECK (redeemed_via IS NULL OR redeemed_via IN ('link', 'code'));

CREATE INDEX idx_magic_link_tokens_nonce
    ON magic_link_tokens(nonce_hash)
    WHERE nonce_hash IS NOT NULL AND used_at IS NULL AND code_spent_at IS NULL;

CREATE INDEX idx_magic_link_tokens_sent
    ON magic_link_tokens(email, sent_at)
    WHERE sent_at IS NOT NULL;
