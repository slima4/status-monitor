-- Turns the public-status-only incident row into a first-class operational
-- incident: an internal state machine (triggered → acknowledged → resolved)
-- orthogonal to the public communication phase, ownership/assignment,
-- escalation bookkeeping, an internal activity log, and a paging delivery log.
--
-- Additive only. No behaviour changes here — the writer/narration/public paths
-- keep working against the existing columns; the new columns default sanely.

-- ── incidents: internal operational columns ──────────────────────────────
ALTER TABLE incidents
    ADD COLUMN state                TEXT NOT NULL DEFAULT 'triggered'
                                    CHECK (state IN ('triggered','acknowledged','resolved')),
    ADD COLUMN urgency              TEXT NOT NULL DEFAULT 'high'
                                    CHECK (urgency IN ('high','low')),
    ADD COLUMN origin               TEXT NOT NULL DEFAULT 'monitor'
                                    CHECK (origin IN ('monitor','manual')),
    ADD COLUMN visibility           TEXT NOT NULL DEFAULT 'internal'
                                    CHECK (visibility IN ('internal','public')),
    ADD COLUMN title                TEXT,            -- internal title; public_title stays separate
    ADD COLUMN acknowledged_at      TIMESTAMPTZ,
    ADD COLUMN acknowledged_by      UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN assigned_to          UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN resolved_by          UUID REFERENCES users(id) ON DELETE SET NULL,  -- NULL ⇒ auto-resolved by writer
    ADD COLUMN escalation_policy_id UUID,            -- FK added once escalation_policies exists
    ADD COLUMN escalation_level     INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN escalation_round     INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN next_escalation_at   TIMESTAMPTZ;     -- NULL ⇒ not escalating (acked / resolved / exhausted)

-- A manually-declared incident need not be tied to a monitor; auto (monitor)
-- incidents always carry their target.
ALTER TABLE incidents ALTER COLUMN target_id DROP NOT NULL;
ALTER TABLE incidents
    ADD CONSTRAINT incident_monitor_has_target
    CHECK (origin = 'manual' OR target_id IS NOT NULL);

-- Backfill existing rows: every incident today is a closed-or-open public
-- status-page incident materialised by the writer.
UPDATE incidents SET state = 'resolved' WHERE ended_at IS NOT NULL;
UPDATE incidents SET visibility = 'public';

-- Escalation worker scan: triggered incidents whose next page is due.
CREATE INDEX idx_incidents_escalation_due
    ON incidents (next_escalation_at)
    WHERE state = 'triggered' AND next_escalation_at IS NOT NULL;

-- Reconcile sweep: triggered incidents that were never paged and never armed
-- (a dropped open signal). Matches due_for_reconcile's predicate so the
-- per-tick scan is index-backed instead of a full table scan.
CREATE INDEX idx_incidents_reconcile
    ON incidents (started_at)
    WHERE state = 'triggered' AND escalation_policy_id IS NULL AND next_escalation_at IS NULL;

-- Org-wide console: list by state, newest first.
CREATE INDEX idx_incidents_org_state_started
    ON incidents (org_id, state, started_at DESC);

-- Reopen lookup: most recently resolved incident for a monitor.
CREATE INDEX idx_incidents_org_target_ended
    ON incidents (org_id, target_id, ended_at DESC) WHERE ended_at IS NOT NULL;

CREATE INDEX idx_incidents_title_trgm ON incidents USING GIN (title gin_trgm_ops);

-- ── incident_events: internal append-only activity log ───────────────────
-- Distinct from incident_updates (the PUBLIC, customer-facing timeline). This
-- is the responder-facing record of every lifecycle action.
CREATE TABLE incident_events (
    id           UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    incident_id  UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    occurred_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind         TEXT NOT NULL CHECK (kind IN (
                   'triggered','acknowledged','assigned','unassigned',
                   'escalated','notified','note','severity_changed',
                   'state_changed','resolved','reopened','published','unpublished',
                   'postmortem_published','postmortem_unpublished'
                 )),
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('system','user','mcp')),
    actor_id     UUID REFERENCES users(id) ON DELETE SET NULL,
    detail       JSONB NOT NULL DEFAULT '{}'::jsonb,
    message      TEXT
);

CREATE INDEX idx_incident_events_org_incident
    ON incident_events (org_id, incident_id, occurred_at);

CREATE TRIGGER trg_incident_events_org_match
    BEFORE INSERT OR UPDATE OF incident_id, org_id ON incident_events
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('incidents', 'incident_id');

-- ── incident_notifications: paging delivery log ──────────────────────────
-- One row per (incident, target, attempt). Powers audit, retry, and dedup.
CREATE TABLE incident_notifications (
    id               UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id           UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    incident_id      UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    escalation_level INTEGER,
    target_user_id   UUID REFERENCES users(id) ON DELETE SET NULL,
    channel_id       UUID REFERENCES notification_channels(id) ON DELETE SET NULL,
    transport        TEXT NOT NULL,
    reason           TEXT NOT NULL CHECK (reason IN ('opened','escalated','resolved','reopened','no_data','data_resumed')),
    status           TEXT NOT NULL CHECK (status IN ('queued','sent','failed','suppressed')),
    attempt          INTEGER NOT NULL DEFAULT 1,
    error            TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at          TIMESTAMPTZ,
    -- Earliest time the retry sweep may re-attempt a failed page (exponential
    -- backoff). NULL = eligible now (never tried, or first failure pre-schedule).
    next_attempt_at  TIMESTAMPTZ
);

CREATE INDEX idx_incident_notifications_incident
    ON incident_notifications (org_id, incident_id, created_at);
-- Retry sweep: pending_notifications orders by next_attempt_at NULLS FIRST so
-- due (and never-scheduled) rows surface first; index the sort key under the
-- same partial predicate so the LIMIT stops early instead of scanning the
-- whole pending set every tick.
CREATE INDEX idx_incident_notifications_retry
    ON incident_notifications (next_attempt_at NULLS FIRST) WHERE status IN ('queued','failed');

CREATE TRIGGER trg_incident_notifications_org_match
    BEFORE INSERT OR UPDATE OF incident_id, org_id ON incident_notifications
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('incidents', 'incident_id');

-- ── targets: per-monitor escalation routing + default severity ───────────
ALTER TABLE targets
    ADD COLUMN escalation_policy_id UUID,            -- FK added once escalation_policies exists
    ADD COLUMN default_severity     TEXT NOT NULL DEFAULT 'major'
                                    CHECK (default_severity IN ('minor','major','critical'));
