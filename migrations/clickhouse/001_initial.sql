-- ClickHouse v1 schema. Multitenant from the start: `org_id` leads the
-- ORDER BY on every tenant table so per-org queries hit the sparse primary
-- index — and stays OUT of the PARTITION BY, since (day, org) would explode
-- into millions of partitions at scale. No DROP statements anywhere — every
-- CREATE is `IF NOT EXISTS`, so a re-run on a populated database is a no-op
-- and a crash between the last CREATE and the `schema_migrations` INSERT
-- can't destroy data on the next boot.

CREATE TABLE IF NOT EXISTS check_results (
    org_id           UUID,
    target_id        UUID,
    region           LowCardinality(String),
    -- Second precision: min check interval is 20s, so no two checks for one
    -- monitor share a second. Sub-second latency lives in `duration_ms`.
    -- DoubleDelta crushes the near-constant interval gaps; jitter-free seconds
    -- make it far tighter than ms would.
    timestamp        DateTime('UTC') CODEC(DoubleDelta, ZSTD(1)),
    -- Server receive time, distinct from agent-supplied `timestamp`, so clock
    -- skew is detectable.
    ingested_at      DateTime('UTC') DEFAULT now() CODEC(DoubleDelta, ZSTD(1)),
    agent_id         LowCardinality(String),
    status           Enum8('up' = 1, 'down' = 2, 'degraded' = 3, 'error' = 4),
    duration_ms      UInt32 CODEC(T64, ZSTD(1)),
    dns_ms           Nullable(UInt16) CODEC(T64, ZSTD(1)),
    connect_ms       Nullable(UInt16) CODEC(T64, ZSTD(1)),
    tls_ms           Nullable(UInt16) CODEC(T64, ZSTD(1)),
    ttfb_ms          Nullable(UInt16) CODEC(T64, ZSTD(1)),
    response_code    Nullable(UInt16) CODEC(T64, ZSTD(1)),
    response_size    Nullable(UInt32) CODEC(T64, ZSTD(1)),
    error            LowCardinality(Nullable(String)),
    -- Per-row retention window, stamped from the org's plan at write time.
    -- DEFAULT applies only until the write path's snapshot first loads. A
    -- per-plan policy change needs no schema change; same-value rows compress
    -- away.
    ttl_days         UInt16 DEFAULT 30 CODEC(ZSTD(1))
) ENGINE = MergeTree
PARTITION BY toYYYYMMDD(timestamp)
ORDER BY (org_id, target_id, region, timestamp)
-- The DEFAULT is the disclosed raw window; Privacy Policy and
-- `tests/retention_test.rs` pin it.
TTL timestamp + toIntervalDay(ttl_days)
-- `non_replicated_deduplication_window`: this is a plain (non-Replicated)
-- MergeTree, where insert dedup is OFF unless this window is set. The batcher
-- re-sends the identical block on retry (`ClickhouseResultSink::write_batch`);
-- without dedup a commit-then-lost-ack would double-count every row. The window
-- (last N block hashes) lets the server drop the duplicate re-send. Sized well
-- past the ~30 s retry budget's worth of sequential batches.
SETTINGS index_granularity = 8192, non_replicated_deduplication_window = 1000;

-- Per-minute pre-aggregation; dashboard rollup merges from here so a
-- 30d / 1k-monitor scan stays O(minutes), not O(raw checks). Ranges past
-- this TTL read the 1h rollup (the disk-heavy minute grain stays short).
CREATE MATERIALIZED VIEW IF NOT EXISTS check_results_1m
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMMDD(minute)
ORDER BY (org_id, target_id, region, minute)
TTL toDateTime(minute) + INTERVAL 30 DAY
AS SELECT
    org_id,
    target_id,
    region,
    toStartOfMinute(timestamp) AS minute,
    countState() AS total_checks,
    countIfState(status = 'up') AS up_checks,
    -- Per-status counts so the day-strip can tell Degraded from Down and the
    -- uptime/summary reads stay O(buckets) instead of scanning raw. up + the
    -- three below sum to total_checks.
    countIfState(status = 'down') AS down_checks,
    countIfState(status = 'degraded') AS degraded_checks,
    countIfState(status = 'error') AS error_checks,
    avgState(duration_ms) AS avg_duration_ms,
    quantilesState(0.5, 0.95, 0.99)(duration_ms) AS duration_quantiles,
    -- Per-phase means power the monitor-detail breakdown chart server-side
    -- (O(buckets), no raw-row pull). Phases are Nullable: avgState skips
    -- NULLs, so non-HTTP checks (tcp/dns) merge to NaN — finalised to 0 in
    -- the query layer.
    avgState(dns_ms) AS avg_dns_ms,
    avgState(connect_ms) AS avg_connect_ms,
    avgState(tls_ms) AS avg_tls_ms,
    avgState(ttfb_ms) AS avg_ttfb_ms,
    argMaxState(status, timestamp) AS last_status_state
FROM check_results
GROUP BY org_id, target_id, region, minute;
