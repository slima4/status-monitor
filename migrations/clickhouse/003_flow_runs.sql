-- One row per browser-flow run. Separate from `check_results` on purpose: that
-- table is the row every check kind writes, and widening it for one kind would
-- put columns nobody else fills on the hot path.

CREATE TABLE IF NOT EXISTS flow_runs (
    org_id          UUID,
    target_id       UUID,
    region          LowCardinality(String),
    timestamp       DateTime('UTC') CODEC(DoubleDelta, ZSTD(1)),
    ingested_at     DateTime('UTC') DEFAULT now() CODEC(DoubleDelta, ZSTD(1)),
    status          Enum8('up' = 1, 'down' = 2, 'degraded' = 3, 'error' = 4),
    duration_ms     UInt32 CODEC(T64, ZSTD(1)),
    -- 0-based index of the step the run stopped at, whether it failed there or
    -- ran out of budget before it; NULL on a passing run. Always the first
    -- non-passed entry of the trace, so it agrees with the error string.
    stopped_step    Nullable(UInt16) CODEC(T64, ZSTD(1)),
    error           String CODEC(ZSTD(1)),
    -- Parallel arrays, one entry per declared step, so a step-level question is
    -- an arrayJoin rather than JSON parsing. One row per run, not per step: the
    -- per-step shape would multiply volume by up to 30 for a query arrayJoin
    -- already serves.
    step_op         Array(LowCardinality(String)),
    step_outcome    Array(Enum8('passed' = 1, 'failed' = 2, 'skipped' = 3)),
    step_ms         Array(UInt32) CODEC(T64, ZSTD(1)),
    -- What the page said behind a failure; empty on a passing run. Content out
    -- of a customer's authenticated session, so the per-column TTL below drops
    -- it ahead of the row that carries it. Past that window a run reads as
    -- trace-present / evidence-absent, which is a normal state.
    final_url       String CODEC(ZSTD(1)) TTL timestamp + toIntervalDay(evidence_days),
    title           String CODEC(ZSTD(1)) TTL timestamp + toIntervalDay(evidence_days),
    text_snippet    String CODEC(ZSTD(1)) TTL timestamp + toIntervalDay(evidence_days),
    console_level   Array(LowCardinality(String)) TTL timestamp + toIntervalDay(evidence_days),
    console_text    Array(String) CODEC(ZSTD(1)) TTL timestamp + toIntervalDay(evidence_days),
    -- Both windows are stamped from the org's plan at write time, same as
    -- `check_results.ttl_days`. A per-plan policy change needs no schema change.
    evidence_days   UInt16 DEFAULT 7 CODEC(ZSTD(1)),
    ttl_days        UInt16 DEFAULT 30 CODEC(ZSTD(1))
) ENGINE = MergeTree
-- Monthly, unlike `check_results`' daily: the flow interval floor is 300s and
-- flow monitors are capped per plan, so daily parts would be many and tiny.
PARTITION BY toYYYYMM(timestamp)
ORDER BY (org_id, target_id, region, timestamp)
TTL timestamp + toIntervalDay(ttl_days)
-- Same reason as `check_results`: this is a plain MergeTree, where insert dedup
-- is off unless the window is set, and the writer re-sends the identical block
-- on retry, which the server then drops by hash instead of double-counting.
SETTINGS index_granularity = 8192, non_replicated_deduplication_window = 1000;
