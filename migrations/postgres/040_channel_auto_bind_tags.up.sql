-- A team owning ten resources otherwise binds its channel to each monitor by
-- hand, and to every new one. The rule routes by tag instead: a channel pages
-- any monitor carrying one of these tags, resolved at alert time so retagging
-- a monitor moves its coverage with no second write.
--
-- No index on the array: the lookup is always org-scoped, the per-org channel
-- count is capped by plan quota, and EXPLAIN takes the org index and filters
-- the handful of rows left. A GIN index here would only cost writes.
ALTER TABLE notification_channels
    ADD COLUMN auto_bind_tags TEXT[] NOT NULL DEFAULT '{}';
