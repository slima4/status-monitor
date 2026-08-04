-- Separate from `check_results`: those are verdicts on our cadence, these
-- arrive on the job's, and a read compares the two.

CREATE TABLE IF NOT EXISTS heartbeat_pings (
    org_id        UUID,
    target_id     UUID,
    received_at   DateTime('UTC') CODEC(DoubleDelta, ZSTD(1)),
    ingested_at   DateTime('UTC') DEFAULT now() CODEC(DoubleDelta, ZSTD(1)),
    signal        Enum8('start' = 1, 'success' = 2, 'fail' = 3),
    -- What `curl $URL/$?` carried; NULL when the signal was a bare word.
    exit_code     Nullable(UInt8) CODEC(T64, ZSTD(1)),
    -- NULL on a start, and on a finish whose start never arrived.
    duration_ms   Nullable(UInt32) CODEC(T64, ZSTD(1)),
    -- Job output. The per-column TTL drops it ahead of the row.
    body          String CODEC(ZSTD(1)) TTL received_at + toIntervalDay(evidence_days),
    -- Stamped from the org's plan at write time, so a policy change needs no DDL.
    evidence_days UInt16 DEFAULT 7 CODEC(ZSTD(1)),
    ttl_days      UInt16 DEFAULT 30 CODEC(ZSTD(1))
) ENGINE = MergeTree
-- Monthly: too few rows per day for daily parts to be worth it.
PARTITION BY toYYYYMM(received_at)
ORDER BY (org_id, target_id, received_at)
TTL received_at + toIntervalDay(ttl_days)
-- Plain MergeTree has no insert dedup unless the window is set, and the writer
-- re-sends the identical block on retry.
SETTINGS index_granularity = 8192, non_replicated_deduplication_window = 1000;
