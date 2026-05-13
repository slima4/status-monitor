ALTER TABLE targets
    ADD COLUMN public_status BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN public_name TEXT,
    ADD COLUMN public_description TEXT,
    ADD COLUMN public_group TEXT,
    ADD COLUMN public_sort_order INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_targets_public
    ON targets(public_status, public_group, public_sort_order)
    WHERE public_status = true;
