-- Regions are config-driven, not seeded here: the control plane upserts its
-- own `scheduler.region` at boot and backfills target assignments to the
-- default region (see AdminRepo::reconcile_regions). A static seed would bake
-- in a name the operator can't pick.
CREATE TABLE regions (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    location    TEXT,
    -- Retire a region without dropping it (FK blocks drop; drop loses history).
    enabled     BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Join from the start: one row = single region, N rows = N regions. Avoids a
-- later column->table migration and its result-dedup semantics change.
CREATE TABLE target_regions (
    target_id   UUID NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    region      TEXT NOT NULL REFERENCES regions(id),
    PRIMARY KEY (target_id, region)
);
CREATE INDEX idx_target_regions_region ON target_regions(region);

-- Operator-tier probes. An agent carries its own bearer credential (no user,
-- so it never lives in api_tokens); auth resolves region + identity from the
-- token and rejects a disabled agent.
CREATE TABLE agents (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    region        TEXT NOT NULL REFERENCES regions(id),
    name          TEXT NOT NULL,
    -- Retire a box without deleting its history; auth rejects a disabled agent.
    enabled       BOOLEAN NOT NULL DEFAULT true,
    -- argon2 hash of the agent's bearer token + its visible prefix for the
    -- lookup narrow (collisions disambiguated by the hash verify).
    token_hash    TEXT NOT NULL,
    token_prefix  TEXT NOT NULL,
    last_seen_at  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_agents_region ON agents(region);
CREATE INDEX idx_agents_token_prefix ON agents(token_prefix);

-- Incident region: NULL = whole-target incident, a value = one region's.
-- Nullable so detection can stay combined now and go per-region later.
ALTER TABLE incidents ADD COLUMN region TEXT;
-- Regions down / still up when the incident opened (drives the alert breakdown,
-- snapshotted so a later assignment change can't rewrite history).
ALTER TABLE incidents ADD COLUMN regions_down TEXT[];
ALTER TABLE incidents ADD COLUMN regions_up TEXT[];

-- Per-plan cap on regions a monitor can be assigned to (i32::MAX = no limit).
ALTER TABLE plans ADD COLUMN max_regions INTEGER NOT NULL DEFAULT 2147483647;

-- Per-monitor detection threshold: "any" | "majority" | "all" | {"count": n}.
ALTER TABLE targets ADD COLUMN region_policy JSONB NOT NULL DEFAULT '"majority"'::jsonb;

-- Per-monitor alerting: consecutive failures before alerting, and whether a
-- recovery is announced. Channels bound to a monitor are pure delivery targets.
ALTER TABLE targets ADD COLUMN alert_confirmations INTEGER NOT NULL DEFAULT 2;
ALTER TABLE targets ADD COLUMN notify_recovery BOOLEAN NOT NULL DEFAULT true;
-- Reminder cadence for an unacknowledged outage: re-page bound channels every
-- N seconds while down. 0 disables the reminder; one page only.
ALTER TABLE targets ADD COLUMN renotify_interval_secs INTEGER NOT NULL DEFAULT 3600;
