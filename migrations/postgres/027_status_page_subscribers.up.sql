-- Public end-user subscriptions to a status page, orthogonal to the
-- operator-internal notification_channels: here the audience is public and
-- every subscription is double opt-in. The (channel, target, config) shape
-- mirrors notification_channels so a new delivery method needs no schema change.
CREATE TABLE status_page_subscribers (
    id             UUID PRIMARY KEY DEFAULT uuidv7(),
    status_page_id UUID NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
    org_id         UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    channel        TEXT NOT NULL CHECK (channel IN ('email', 'webhook', 'slack', 'sms')),
    -- Caller normalises (e.g. lowercases email) so the unique index folds case variants.
    target         TEXT NOT NULL,
    config         JSONB NOT NULL DEFAULT '{}'::jsonb,
    verified_at    TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX idx_status_page_subscribers_unique
    ON status_page_subscribers (status_page_id, channel, target);

CREATE INDEX idx_status_page_subscribers_page_verified
    ON status_page_subscribers (status_page_id, channel)
    WHERE verified_at IS NOT NULL;

CREATE TRIGGER trg_status_page_subscribers_org_match
    BEFORE INSERT OR UPDATE OF status_page_id, org_id ON status_page_subscribers
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('status_pages', 'status_page_id');

-- Single-use confirm tokens for double opt-in; only the SHA-256 is stored.
-- consume re-checks target so an address change burns older tokens.
CREATE TABLE status_page_subscriber_tokens (
    id            UUID PRIMARY KEY DEFAULT uuidv7(),
    subscriber_id UUID NOT NULL REFERENCES status_page_subscribers(id) ON DELETE CASCADE,
    org_id        UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    target        TEXT NOT NULL,
    token_hash    TEXT NOT NULL UNIQUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL,
    used_at       TIMESTAMPTZ
);

CREATE INDEX idx_status_page_subscriber_tokens_subscriber
    ON status_page_subscriber_tokens (subscriber_id, created_at);
CREATE INDEX idx_status_page_subscriber_tokens_org
    ON status_page_subscriber_tokens (org_id, created_at);
