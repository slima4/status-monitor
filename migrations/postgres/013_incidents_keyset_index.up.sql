-- Covers the public-page incident keyset scan in `OrgPublicSource::list_incidents`:
--   WHERE org_id = $org
--     AND started_at >= $since
--     AND (started_at, id) < ($cursor_ts, $cursor_id)
--   ORDER BY started_at DESC, id DESC
--   LIMIT n+1
--
-- The previous `idx_incidents_org_target_started (org_id, target_id,
-- started_at DESC)` is great when the predicate names a target_id, but the
-- public list pivots only on org_id + started_at + id, so without a leading
-- `(org_id, started_at DESC, id DESC)` index Postgres falls back to a heap
-- scan + sort. Tiebreaker on id keeps the keyset stable across rows that
-- share a started_at.
CREATE INDEX IF NOT EXISTS idx_incidents_org_started_id_desc
    ON incidents (org_id, started_at DESC, id DESC);
