CREATE TABLE targets (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT NOT NULL,
    check_spec      JSONB NOT NULL,
    interval_secs   INTEGER NOT NULL CHECK (interval_secs >= 10),
    enabled         BOOLEAN NOT NULL DEFAULT true,
    tags            TEXT[] NOT NULL DEFAULT '{}',
    alerts          JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_targets_enabled ON targets(enabled) WHERE enabled = true;
CREATE INDEX idx_targets_tags ON targets USING GIN(tags);
CREATE INDEX idx_targets_updated ON targets(updated_at);
