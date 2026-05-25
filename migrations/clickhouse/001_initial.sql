-- ClickHouse v1 schema. Multitenant from the start: `org_id` leads the
-- ORDER BY on every tenant table so per-org queries hit the sparse primary
-- index. No DROP statements anywhere — every CREATE is `IF NOT EXISTS`, so a
-- re-run on a populated database is a no-op and a process crash between the
-- last CREATE and the `schema_migrations` INSERT can't destroy data on the
-- next boot.

CREATE TABLE IF NOT EXISTS check_results (
    org_id           UUID,
    target_id        UUID,
    timestamp        DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    status           Enum8('up' = 1, 'down' = 2, 'degraded' = 3, 'error' = 4),
    duration_ms      UInt32 CODEC(T64, ZSTD(1)),
    dns_ms           Nullable(UInt16),
    connect_ms       Nullable(UInt16),
    tls_ms           Nullable(UInt16),
    ttfb_ms          Nullable(UInt16),
    response_code    Nullable(UInt16),
    response_size    Nullable(UInt32),
    error            LowCardinality(Nullable(String))
) ENGINE = MergeTree
PARTITION BY (toYYYYMMDD(timestamp), org_id)
ORDER BY (org_id, target_id, timestamp)
-- 90-day retention matches the public status page's daily strip
-- (`history_days` config) and the per-target operator drilldown window.
-- The Privacy Policy and `tests/retention_test.rs` pin the same number;
-- changing it here must update both.
TTL toDateTime(timestamp) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

-- Per-minute pre-aggregation; dashboard rollup merges from here so a
-- 90d / 1k-monitor scan stays O(minutes), not O(raw checks).
CREATE MATERIALIZED VIEW IF NOT EXISTS check_results_1m
ENGINE = AggregatingMergeTree
PARTITION BY (toYYYYMMDD(minute), org_id)
ORDER BY (org_id, target_id, minute)
TTL toDateTime(minute) + INTERVAL 90 DAY
AS SELECT
    org_id,
    target_id,
    toStartOfMinute(timestamp) AS minute,
    countState() AS total_checks,
    countIfState(status = 'up') AS up_checks,
    avgState(duration_ms) AS avg_duration_ms,
    quantilesState(0.5, 0.95, 0.99)(duration_ms) AS duration_quantiles,
    argMaxState(status, timestamp) AS last_status_state
FROM check_results
GROUP BY org_id, target_id, minute;
