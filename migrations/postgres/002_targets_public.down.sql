DROP INDEX IF EXISTS idx_targets_public;

ALTER TABLE targets
    DROP COLUMN IF EXISTS public_sort_order,
    DROP COLUMN IF EXISTS public_group,
    DROP COLUMN IF EXISTS public_description,
    DROP COLUMN IF EXISTS public_name,
    DROP COLUMN IF EXISTS public_status;
