-- Quotas & plan model. Schema only — no enforcement path reads these tables
-- yet. `plans` is the single source of truth for resource limits and per-org
-- rate limits; adding a paid tier later is one INSERT plus a UI page.

CREATE TABLE plans (
    id                              TEXT PRIMARY KEY,
    name                            TEXT NOT NULL,
    description                     TEXT NOT NULL,
    -- Resource quotas
    max_targets                     INTEGER NOT NULL,
    min_check_interval_secs         INTEGER NOT NULL,
    retention_days                  INTEGER NOT NULL,
    max_members                     INTEGER NOT NULL,
    max_pending_invitations         INTEGER NOT NULL,
    max_api_tokens_per_user         INTEGER NOT NULL,
    max_public_components           INTEGER NOT NULL,
    max_maintenance_windows         INTEGER NOT NULL,
    max_notification_channels       INTEGER NOT NULL,
    max_logo_size_bytes             INTEGER NOT NULL,
    -- Per-org rate limits (per minute)
    api_writes_per_minute           INTEGER NOT NULL,
    api_reads_per_minute            INTEGER NOT NULL,
    bulk_ops_per_minute             INTEGER NOT NULL,
    test_now_per_minute             INTEGER NOT NULL,
    check_now_per_minute            INTEGER NOT NULL,
    -- Feature toggles (wired in a later phase)
    custom_domain_enabled           BOOLEAN NOT NULL DEFAULT false,
    white_label_enabled             BOOLEAN NOT NULL DEFAULT false,
    incident_narration_enabled      BOOLEAN NOT NULL DEFAULT true,
    -- Metadata
    is_listed                       BOOLEAN NOT NULL DEFAULT true,
    created_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Seed the free plan. Numbers are generous for a small team yet keep abuse
-- on a tiny VM bounded. Inserted before the organizations FK below so the
-- column default resolves for any pre-existing rows.
INSERT INTO plans (
    id, name, description,
    max_targets, min_check_interval_secs, retention_days,
    max_members, max_pending_invitations, max_api_tokens_per_user,
    max_public_components, max_maintenance_windows, max_notification_channels,
    max_logo_size_bytes,
    api_writes_per_minute, api_reads_per_minute,
    bulk_ops_per_minute, test_now_per_minute, check_now_per_minute,
    incident_narration_enabled
) VALUES (
    'free', 'Free', 'Free tier for small teams and personal projects',
    10, 60, 30,
    5, 10, 5,
    10, 20, 20,
    204800,
    600, 6000,
    30, 60, 60,
    true
);

-- Every org references a plan. Existing rows backfill to 'free' via the
-- default; the FK makes removing or renaming a referenced plan impossible
-- (no silent data loss). The billing webhook flips this column in a later
-- phase.
ALTER TABLE organizations
    ADD COLUMN plan_id TEXT NOT NULL DEFAULT 'free' REFERENCES plans(id);
CREATE INDEX idx_organizations_plan ON organizations(plan_id);

-- Per-org non-standard limits (beta customers, friends-of-the-project).
-- The table exists now; no read path consults it yet. When it does, it is a
-- single JSON merge in the limit lookup.
CREATE TABLE plan_overrides (
    org_id          UUID PRIMARY KEY REFERENCES organizations(id) ON DELETE CASCADE,
    override_json   JSONB NOT NULL DEFAULT '{}'::jsonb,
    reason          TEXT NOT NULL,                     -- audit trail
    set_by_user_id  UUID REFERENCES users(id) ON DELETE SET NULL,
    expires_at      TIMESTAMPTZ,                       -- nullable = permanent
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Append-only audit of quota / rate-limit / abuse events. Retention is 90
-- days, purged by the existing daily job pattern in a later phase.
CREATE TABLE quota_events (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID REFERENCES organizations(id) ON DELETE CASCADE,
    user_id         UUID REFERENCES users(id) ON DELETE SET NULL,
    event           TEXT NOT NULL,           -- 'quota_exceeded', 'rate_limited', 'abuse_blocked'
    quota_name      TEXT,                    -- 'max_targets', 'api_writes_per_minute', etc.
    details         JSONB NOT NULL DEFAULT '{}'::jsonb,
    ip_hash         TEXT,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_quota_events_org_time
    ON quota_events(org_id, occurred_at DESC);

-- Partial index for abuse review. The recency window is intentionally NOT in
-- the predicate: now() is not IMMUTABLE so Postgres rejects it in an index
-- WHERE clause. Callers add the `occurred_at > now() - interval '30 days'`
-- filter at query time; the planner still uses this partial index for
-- selectivity. Same pattern as idx_login_attempts_recent_failures.
CREATE INDEX idx_quota_events_recent_abuse
    ON quota_events(occurred_at DESC, ip_hash)
    WHERE event = 'abuse_blocked';

-- Full occurred_at index for the daily retention delete (the indexes above
-- are partial / org_id-leading and can't serve an unfiltered range delete).
CREATE INDEX idx_quota_events_occurred_at ON quota_events(occurred_at);
