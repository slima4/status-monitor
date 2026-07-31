-- Per-plan limits for browser flow monitors.
--
-- `evidence_days`: a failed run keeps what the page showed, which is content
-- from behind the customer's own login, so it is held on a shorter clock than
-- the run itself.
-- `max_flow_steps`: a longer journey costs more browser time per run on a
-- shared probe, so its length is a plan lever rather than one number for
-- everyone. Clamped to the engine ceiling, so raising it past that does nothing.
ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS evidence_days  INTEGER NOT NULL DEFAULT 7,
    ADD COLUMN IF NOT EXISTS max_flow_steps INTEGER NOT NULL DEFAULT 30;
