CREATE TABLE incidents (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
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

CREATE INDEX idx_incidents_target_started ON incidents(target_id, started_at DESC);
CREATE INDEX idx_incidents_open ON incidents(target_id) WHERE ended_at IS NULL;
CREATE INDEX idx_incidents_started ON incidents(started_at DESC);

CREATE TABLE incident_updates (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id  UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    posted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    phase        TEXT NOT NULL
                 CHECK (phase IN ('investigating','identified','monitoring','resolved','postmortem')),
    message      TEXT NOT NULL,
    author       TEXT
);

CREATE INDEX idx_incident_updates_incident ON incident_updates(incident_id, posted_at);

CREATE TABLE maintenance_windows (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title       TEXT NOT NULL,
    description TEXT,
    starts_at   TIMESTAMPTZ NOT NULL,
    ends_at     TIMESTAMPTZ NOT NULL CHECK (ends_at > starts_at),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_maintenance_window_range ON maintenance_windows(starts_at, ends_at);

CREATE TABLE maintenance_window_components (
    maintenance_id UUID NOT NULL REFERENCES maintenance_windows(id) ON DELETE CASCADE,
    target_id      UUID NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    PRIMARY KEY (maintenance_id, target_id)
);

CREATE INDEX idx_maintenance_window_components_target
    ON maintenance_window_components(target_id);
