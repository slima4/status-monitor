-- Adding or removing a sign-in method is the one account change an attacker
-- holding a session would want to make quietly, and the mail announcing it is
-- best-effort. Not `login_attempts`: its `success` column drives
-- credential-stuffing detection, which these rows would distort.
CREATE TABLE credential_events (
    id               UUID NOT NULL DEFAULT uuidv7(),
    user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider         TEXT NOT NULL,
    -- Kept after the identity row is gone; that is the whole point.
    provider_user_id TEXT NOT NULL,
    action           TEXT NOT NULL CHECK (action IN ('linked', 'unlinked')),
    -- `email_match` is the one worth investigating: a provider let itself in
    -- on an attested address and nobody clicked add.
    origin           TEXT NOT NULL CHECK (origin IN ('signup', 'email_match', 'session')),
    -- Salted like `login_attempts`, so the two trails can be compared.
    ip_hash          TEXT,
    user_agent_hash  TEXT,
    metadata         JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);
CREATE TABLE credential_events_default PARTITION OF credential_events DEFAULT;
CREATE INDEX idx_credential_events_user_time
    ON credential_events(user_id, occurred_at DESC);
CREATE INDEX idx_credential_events_occurred_at ON credential_events(occurred_at);

-- Without this an account older than the table reads as "nothing was ever
-- added", which is the answer that ends an investigation early.
INSERT INTO credential_events
    (user_id, provider, provider_user_id, action, origin, occurred_at)
SELECT user_id, provider, provider_user_id, 'linked', 'signup', created_at
  FROM oauth_identities;
