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
-- delete cascades the cache.
CREATE TABLE IF NOT EXISTS domain_expiry_state (
    target_id        UUID PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    domain           TEXT NOT NULL,
    expiry_at        TIMESTAMPTZ NOT NULL,
    registrar        TEXT,
    verified_at      TIMESTAMPTZ NOT NULL,
    last_attempt_at  TIMESTAMPTZ NOT NULL,
    last_error       TEXT,
    attempts         INT NOT NULL DEFAULT 0
);
