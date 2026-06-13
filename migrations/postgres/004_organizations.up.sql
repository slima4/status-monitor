CREATE EXTENSION IF NOT EXISTS citext;
-- Trigram GIN indexes back the console's substring search (`name ILIKE
-- '%q%'` on targets, `title ILIKE` on incidents) — a leading wildcard can't
-- use a btree, so without this every keystroke is a per-org seq scan.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE organizations (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Partial-UNIQUE via `idx_organizations_active` so a soft-deleted slug
    -- frees up for re-signup. Full UNIQUE would pin it to the tombstone.
    slug        CITEXT NOT NULL,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ,

    CONSTRAINT slug_format CHECK (
        slug ~ '^[a-z][a-z0-9-]{1,28}[a-z0-9]$'
        AND slug NOT LIKE '%--%'
    )
);

CREATE UNIQUE INDEX idx_organizations_active
    ON organizations(slug)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_organizations_pending_purge
    ON organizations(deleted_at)
    WHERE deleted_at IS NOT NULL;

-- Column order: fixed-width 8-byte-aligned (UUID, TIMESTAMPTZ) clustered
-- first, variable-length (CITEXT, TEXT) in the middle, audit timestamps
-- (created/updated/deleted) at the end by convention.
CREATE TABLE users (
    id                      UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Nullable so the user row can land before its signup org exists in
    -- the same tx; the signup path UPDATEs this once the org commits.
    signup_org_id           UUID        REFERENCES organizations(id) ON DELETE SET NULL,
    onboarding_completed_at TIMESTAMPTZ,
    last_seen_at            TIMESTAMPTZ,
    email_verified_at       TIMESTAMPTZ,
    -- `*_version` has no default — signup binds `auth::consent::*_VERSION`
    -- explicitly; a baked-in `'v1'` would lie once the constant moves on.
    terms_accepted_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    privacy_accepted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    marketing_opt_in_at     TIMESTAMPTZ,
    -- Partial-UNIQUE via `idx_users_active`; see `organizations.slug` above.
    email                   CITEXT      NOT NULL,
    display_name            TEXT,
    theme                   TEXT        NOT NULL DEFAULT 'default'
                            CHECK (theme IN (
                                'default', 'terminal', 'winter',
                                'dark', 'night', 'dim', 'nord', 'dracula',
                                'corporate', 'light', 'cupcake', 'cyberpunk', 'synthwave'
                            )),
    -- `locale` (IETF BCP-47) and `timezone` (IANA) are unvalidated by SQL;
    -- the Rust setter rejects garbage so the email renderer can trust the
    -- value. See `domain::preferences`.
    locale                  TEXT,
    timezone                TEXT,
    time_format             TEXT        NOT NULL DEFAULT 'auto'
                            CHECK (time_format IN ('auto', '12h', '24h')),
    terms_version           TEXT        NOT NULL,
    privacy_version         TEXT        NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at              TIMESTAMPTZ
);

CREATE UNIQUE INDEX idx_users_active ON users(email) WHERE deleted_at IS NULL;

CREATE TABLE memberships (
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    role        TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, org_id)
);

CREATE INDEX idx_memberships_org ON memberships(org_id);
CREATE INDEX idx_memberships_user ON memberships(user_id);

-- Compliance-grade audit trail. Every row is written in-transaction with
-- its data change via `storage::orgs::record_audit_tx` (the single writer,
-- fenced by `scripts/sg-rules/org_audit_single_writer.yml`).
-- Month-partitioned; the PK must include the partition key.
CREATE TABLE org_audit_log (
    id          UUID NOT NULL DEFAULT uuidv7(),
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    actor_id    UUID REFERENCES users(id) ON DELETE SET NULL,
    action      TEXT NOT NULL,
    metadata    JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);

CREATE TABLE org_audit_log_default PARTITION OF org_audit_log DEFAULT;

CREATE INDEX idx_audit_log_org_time ON org_audit_log(org_id, occurred_at DESC);
-- org_id-leading index above can't serve the boundary retention delete
-- (`occurred_at < cutoff`, no org filter) — add a plain one.
CREATE INDEX idx_org_audit_log_occurred_at ON org_audit_log(occurred_at);

CREATE TABLE clickhouse_purge_queue (
    org_id       UUID PRIMARY KEY,
    queued_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    completed_at TIMESTAMPTZ
);

CREATE INDEX idx_clickhouse_purge_pending
    ON clickhouse_purge_queue(queued_at)
    WHERE completed_at IS NULL;
