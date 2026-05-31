-- Shareable single-monitor links: a per-monitor capability URL (/m/{token})
-- that renders the monitor's read-only detail view to anyone with the link,
-- no account. The token is a 256-bit random shown once; only its SHA-256 is
-- stored, unique-indexed, and resolved by hash. Revoke + optional expiry are
-- the controls. A monitor may have several shares.
CREATE TABLE monitor_shares (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    target_id    UUID NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    -- SHA-256 hex of the token (auth::sha256_hex), mirroring sessions.id_hash /
    -- oauth_states.state_hash. A 256-bit URL-safe random has no brute-force
    -- surface, so a fast preimage-resistant hash is the right tool — argon2 is
    -- for the low-entropy human-typed API tokens.
    token_hash   TEXT NOT NULL UNIQUE,
    -- Reversible copy of the token so the owner can re-copy the link later
    -- (share-link UX, like Docs/Dropbox). Encrypted with the app KEK when
    -- configured (the same Cipher envelope as basic_auth/bearer_token), else
    -- stored plaintext. The public resolve path never reads this — it matches
    -- on token_hash — so a hot link never triggers a decrypt.
    token_enc    TEXT NOT NULL,
    label        TEXT,
    -- Audit-only; the share keeps working after the creator leaves the org.
    created_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- NULL = never expires.
    expires_at   TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ,
    -- Rough "how much is this link used" stat, bumped on each page view of the
    -- shared monitor (not on the live/chart sub-resource polls). Vanity metric
    -- shown to the operator; never gates anything.
    view_count     BIGINT NOT NULL DEFAULT 0,
    last_viewed_at TIMESTAMPTZ,
    view_style          TEXT NOT NULL DEFAULT 'default',
    custom_css          TEXT,
    custom_css_compiled TEXT,
    custom_css_enabled  BOOLEAN NOT NULL DEFAULT false,

    CONSTRAINT monitor_shares_label_length
        CHECK (label IS NULL OR char_length(label) BETWEEN 1 AND 80),
    CONSTRAINT monitor_shares_view_style_known
        CHECK (view_style IN (
            'default', 'classic', 'terminal', 'winter',
            'dark', 'night', 'dim', 'nord', 'dracula',
            'corporate', 'light', 'cupcake', 'cyberpunk', 'synthwave'
        )),
    CONSTRAINT monitor_shares_custom_css_length
        CHECK (custom_css IS NULL OR char_length(custom_css) <= 20000)
);

-- Operator list (shares for one monitor). The UNIQUE token_hash covers resolve.
CREATE INDEX idx_monitor_shares_target ON monitor_shares (org_id, target_id);

-- The share's org must equal its target's org (belt-and-suspenders past the
-- type-enforced repository layer).
CREATE TRIGGER trg_monitor_shares_target_org
    BEFORE INSERT OR UPDATE OF target_id, org_id ON monitor_shares
    FOR EACH ROW EXECUTE FUNCTION assert_target_in_same_org();
