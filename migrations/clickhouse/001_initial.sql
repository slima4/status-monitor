CREATE TABLE IF NOT EXISTS check_results (
    target_id UUID,
    timestamp DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    status Enum8('up' = 1, 'down' = 2, 'degraded' = 3, 'error' = 4),
    duration_ms UInt32 CODEC(T64, ZSTD(1)),
    dns_ms Nullable(UInt16),
    connect_ms Nullable(UInt16),
    tls_ms Nullable(UInt16),
    ttfb_ms Nullable(UInt16),
    response_code Nullable(UInt16),
    response_size Nullable(UInt32),
    error LowCardinality(Nullable(String))
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(timestamp)
ORDER BY (target_id, timestamp)
TTL toDateTime(timestamp) + INTERVAL 30 DAY
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS check_results_1m
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMMDD(minute)
ORDER BY (target_id, minute)
AS SELECT
    target_id,
    toStartOfMinute(timestamp) AS minute,
    countState() AS total_checks,
    countIfState(status = 'up') AS up_checks,
    avgState(duration_ms) AS avg_duration_ms,
    quantileState(0.99)(duration_ms) AS p99_duration_ms
FROM check_results
GROUP BY target_id, minute;
