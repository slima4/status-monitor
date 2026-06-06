-- Hour-rollup of check_results (long history tail). Design + read-routing
-- rationale lives at the MIGRATIONS const in src/storage/clickhouse.rs — this
-- file is frozen once shipped.
CREATE MATERIALIZED VIEW IF NOT EXISTS check_results_1h
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(hour)
ORDER BY (org_id, target_id, region, hour)
TTL toDateTime(hour) + INTERVAL 13 MONTH
AS SELECT
    org_id,
    target_id,
    region,
    toStartOfHour(timestamp) AS hour,
    countState() AS total_checks,
    countIfState(status = 'up') AS up_checks,
    countIfState(status = 'down') AS down_checks,
    countIfState(status = 'degraded') AS degraded_checks,
    countIfState(status = 'error') AS error_checks,
    avgState(duration_ms) AS avg_duration_ms,
    quantilesState(0.5, 0.95, 0.99)(duration_ms) AS duration_quantiles,
    avgState(dns_ms) AS avg_dns_ms,
    avgState(connect_ms) AS avg_connect_ms,
    avgState(tls_ms) AS avg_tls_ms,
    avgState(ttfb_ms) AS avg_ttfb_ms,
    argMaxState(status, timestamp) AS last_status_state
FROM check_results
GROUP BY org_id, target_id, region, hour;
