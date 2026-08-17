-- Structured explanation for a failed check, stored beside the protocol error
-- rather than replacing it: status and assertions stay authoritative. Only the
-- observation is kept; remediation advice is derived from the kind on read.

ALTER TABLE check_results
    ADD COLUMN IF NOT EXISTS diagnostic_kind LowCardinality(Nullable(String)) AFTER error;

ALTER TABLE check_results
    ADD COLUMN IF NOT EXISTS diagnostic_confidence LowCardinality(Nullable(String)) AFTER diagnostic_kind;

ALTER TABLE check_results
    ADD COLUMN IF NOT EXISTS diagnostic_provider LowCardinality(Nullable(String)) AFTER diagnostic_confidence;

-- Four-value vocabulary, so the dictionary stays tiny across every row.
ALTER TABLE check_results
    ADD COLUMN IF NOT EXISTS diagnostic_evidence Array(LowCardinality(String)) AFTER diagnostic_provider;
