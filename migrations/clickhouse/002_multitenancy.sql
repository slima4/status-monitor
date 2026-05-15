-- Greenfield rewrite: drop existing single-tenant check_results and recreate
-- with org_id as the leading ORDER BY column so per-org queries hit the sparse
-- primary index. Existing dev/test data is discarded.

-- SYNC forces synchronous removal under the Atomic database engine. Without
-- it the CREATE MATERIALIZED VIEW below races the catalog and resolves
-- `check_results` against the in-flight tombstone instead of the new table
-- (observed in CI).
DROP VIEW IF EXISTS check_results_1m SYNC;
DROP TABLE IF EXISTS check_results SYNC;

CREATE TABLE check_results (
    org_id     UUID,
    target_id  UUID,
    timestamp  DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    status     Enum8('up' = 1, 'down' = 2, 'degraded' = 3, 'error' = 4),
    duration_ms UInt32 CODEC(T64, ZSTD(1)),
    dns_ms      Nullable(UInt16),
    connect_ms  Nullable(UInt16),
    tls_ms      Nullable(UInt16),
    ttfb_ms     Nullable(UInt16),
    response_code Nullable(UInt16),
    response_size Nullable(UInt32),
    error       LowCardinality(Nullable(String))
) ENGINE = MergeTree
PARTITION BY (toYYYYMMDD(timestamp), org_id)
ORDER BY (org_id, target_id, timestamp)
TTL toDateTime(timestamp) + INTERVAL 90 DAY
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW check_results_1m
ENGINE = AggregatingMergeTree
PARTITION BY (toYYYYMMDD(minute), org_id)
ORDER BY (org_id, target_id, minute)
AS SELECT
    org_id,
    target_id,
    toStartOfMinute(timestamp) AS minute,
    countState() AS total_checks,
    countIfState(status = 'up') AS up_checks,
    avgState(duration_ms) AS avg_duration_ms,
    quantileState(0.99)(duration_ms) AS p99_duration_ms
FROM check_results
GROUP BY org_id, target_id, minute;
