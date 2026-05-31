-- Greenfield rewrite: drop existing single-tenant tenant tables and recreate
-- them with org_id NOT NULL on every row (denormalised on children so isolation
-- checks are always WHERE org_id = $1, never a parent-join).

DROP TABLE IF EXISTS maintenance_window_components;
DROP TABLE IF EXISTS maintenance_windows;
DROP TABLE IF EXISTS incident_updates;
DROP TABLE IF EXISTS incidents;
DROP TABLE IF EXISTS targets;

CREATE TABLE targets (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name                  TEXT NOT NULL,
    check_spec            JSONB NOT NULL,
    -- `>= 10` is an ingest-rate backstop, not an arbitrary minimum: lowering it
    -- can push sustained ingest past what the batcher buffer absorbs (silent
    -- buffer_overflow drops). Plans tighten it via `plans.min_check_interval_secs`.
    interval_secs         INTEGER NOT NULL CHECK (interval_secs >= 10),
    enabled               BOOLEAN NOT NULL DEFAULT true,
    tags                  TEXT[] NOT NULL DEFAULT '{}',
    alerts                JSONB NOT NULL DEFAULT '[]'::jsonb,
    group_name            TEXT,
    owner_user_id         UUID REFERENCES users(id) ON DELETE SET NULL,
    public_status         BOOLEAN NOT NULL DEFAULT false,
    public_name           TEXT,
    public_description    TEXT,
    public_group          TEXT,
    public_sort_order     INTEGER NOT NULL DEFAULT 0,
    write_source          TEXT NOT NULL DEFAULT 'ui'
                          CHECK (write_source IN ('ui', 'api', 'terraform')),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_targets_org_enabled
    ON targets(org_id, enabled) WHERE enabled = true;
CREATE INDEX idx_targets_org_updated ON targets(org_id, updated_at);
CREATE INDEX idx_targets_org_tags ON targets USING GIN(tags) WHERE tags <> '{}';
CREATE INDEX idx_targets_org_group
    ON targets(org_id, group_name) WHERE group_name IS NOT NULL;
CREATE INDEX idx_targets_org_owner
    ON targets(org_id, owner_user_id) WHERE owner_user_id IS NOT NULL;
CREATE INDEX idx_targets_org_public
    ON targets(org_id, public_group, public_sort_order)
    WHERE public_status = true;
-- Keyset cursor index for cross-tenant pagination by (org_id, id).
-- Partial scope keeps the index narrow (live public-status targets only)
-- and lets Postgres range-scan it directly for the writer's per-page query.
CREATE INDEX idx_targets_public_page_cursor
    ON targets(org_id, id)
    WHERE enabled = true AND public_status = true;

CREATE TABLE incidents (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    target_id             UUID NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    started_at            TIMESTAMPTZ NOT NULL,
    ended_at              TIMESTAMPTZ,
    severity              TEXT NOT NULL DEFAULT 'major'
                          CHECK (severity IN ('minor', 'major', 'critical')),
    status_at_start       TEXT NOT NULL
                          CHECK (status_at_start IN ('down', 'degraded', 'error')),
    check_count           INTEGER NOT NULL DEFAULT 0,
    error_sample          TEXT,
    public_title          TEXT,
    public_description    TEXT,
    duration_secs         INTEGER,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_incidents_org_target_started
    ON incidents(org_id, target_id, started_at DESC);
CREATE INDEX idx_incidents_org_open
    ON incidents(org_id, target_id) WHERE ended_at IS NULL;

CREATE TABLE incident_updates (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    incident_id     UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    posted_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    phase           TEXT NOT NULL
                    CHECK (phase IN ('investigating', 'identified', 'monitoring', 'resolved', 'postmortem')),
    message         TEXT NOT NULL,
    author          TEXT
);

CREATE INDEX idx_incident_updates_org_incident
    ON incident_updates(org_id, incident_id, posted_at);

CREATE TABLE maintenance_windows (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    title           TEXT NOT NULL,
    description     TEXT,
    starts_at       TIMESTAMPTZ NOT NULL,
    ends_at         TIMESTAMPTZ NOT NULL CHECK (ends_at > starts_at),
    write_source    TEXT NOT NULL DEFAULT 'ui'
                    CHECK (write_source IN ('ui', 'api', 'terraform')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_maintenance_org_range
    ON maintenance_windows(org_id, starts_at, ends_at);

CREATE TABLE maintenance_window_components (
    org_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    maintenance_id  UUID NOT NULL REFERENCES maintenance_windows(id) ON DELETE CASCADE,
    target_id       UUID NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    PRIMARY KEY (maintenance_id, target_id)
);

CREATE INDEX idx_maintenance_components_org_target
    ON maintenance_window_components(org_id, target_id);
