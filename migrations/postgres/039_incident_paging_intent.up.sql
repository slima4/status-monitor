-- Whether this incident is allowed to page. The reconcile sweep pages any
-- triggered incident that reached no channel, so a quiet declare's intent has
-- to outlive the request that carried it rather than gate the first signal.
ALTER TABLE incidents
    ADD COLUMN paging_enabled BOOLEAN NOT NULL DEFAULT true;
