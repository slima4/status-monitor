-- Sticky last-good cache for domain_expiry checks. Probing a registry's RDAP
-- endpoint costs an outbound HTTP round trip and registries rate-limit, so a
-- single transient miss (timeout, 429, throttle) must not flip the customer's
-- monitor from "Up — expires in 47 days" to a Degraded incident. The worker
-- writes the most recent successful (domain, expiry, registrar) here on each
-- successful probe and reads it back when the next probe fails or is
-- throttled, falling through to a real Error only once the cached row is
-- older than the staleness ceiling.
--
-- One row per target. `target_id` is the PK so an upsert is O(1) and a target
-- delete cascades the cache. `org_id` is denormalised onto the row so every
-- query can filter by tenant — the trait method signatures require OrgId, so
-- a future HTTP handler that takes target_id from request input cannot read
-- another tenant's row by mistake.
--
-- `last_success_at` is the moment of the last *successful* probe. Distinct
-- from `last_attempt_at`, which advances on every probe (success or failure).
-- The staleness ceiling is measured against `last_success_at` only — a row
-- with no successful probe must never satisfy the staleness branch.
CREATE TABLE IF NOT EXISTS domain_expiry_state (
    target_id        UUID PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    org_id           UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    domain           TEXT NOT NULL,
    expiry_at        TIMESTAMPTZ NOT NULL,
    registrar        TEXT,
    last_success_at  TIMESTAMPTZ NOT NULL,
    last_attempt_at  TIMESTAMPTZ NOT NULL,
    last_error       TEXT,
    attempts         INT NOT NULL DEFAULT 0
);

-- Row's org_id must match the target's, blocking a cross-tenant read/write via
-- a request-supplied target_id.
CREATE TRIGGER trg_domain_expiry_state_target_org
    BEFORE INSERT OR UPDATE OF target_id, org_id ON domain_expiry_state
    FOR EACH ROW EXECUTE FUNCTION assert_target_in_same_org();
