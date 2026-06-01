-- Per-status-page resources (logo now; background/favicon/css later) stored
-- inline as BYTEA. The `storage`/`external_key` pair is the seam to move bytes
-- to S3 later without a schema change.
CREATE TABLE page_assets (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    status_page_id  uuid NOT NULL REFERENCES status_pages(id)  ON DELETE CASCADE,
    slot            text NOT NULL,
    content_type    text NOT NULL,
    content_hash    text NOT NULL,
    byte_size       bigint NOT NULL,
    storage         text  NOT NULL DEFAULT 'db' CHECK (storage IN ('db','s3')),
    data            bytea,
    external_key    text,
    metadata        jsonb NOT NULL DEFAULT '{}',
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (status_page_id, slot),
    CONSTRAINT page_assets_storage_consistent CHECK (
        (storage='db' AND data IS NOT NULL AND external_key IS NULL) OR
        (storage='s3' AND data IS NULL     AND external_key IS NOT NULL))
);

CREATE INDEX page_assets_page_idx ON page_assets (status_page_id);
