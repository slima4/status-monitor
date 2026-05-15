-- Per-org public status page branding columns. Master switch defaults to
-- false; `ensure_default_org` flips it to true on first INSERT and the
-- backfill below covers existing self-host deployments with the canonical
-- `default` slug.

ALTER TABLE organizations
    ADD COLUMN public_status_enabled       BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN public_display_name         TEXT,
    ADD COLUMN public_about                TEXT,
    ADD COLUMN public_brand_color          TEXT,
    ADD COLUMN public_logo_path            TEXT,
    -- Nullable on purpose: the domain models this as Option<bool> where NULL
    -- means "no operator override — fall back to config.default_show_powered_by
    -- at read time", the same NULL-is-fallback contract every other public_*
    -- column above uses. A NOT NULL DEFAULT here would make that tri-state
    -- unrepresentable and turn a legitimate `None` write into a constraint
    -- violation. For the default config NULL still resolves to `true`.
    ADD COLUMN public_show_powered_by      BOOLEAN,
    ADD COLUMN public_custom_domain        TEXT UNIQUE,
    ADD COLUMN public_custom_domain_verified_at TIMESTAMPTZ;

ALTER TABLE organizations
    ADD CONSTRAINT brand_color_format
        CHECK (public_brand_color IS NULL
               OR public_brand_color ~ '^#[0-9a-fA-F]{6}$'),
    ADD CONSTRAINT about_length
        CHECK (public_about IS NULL
               OR char_length(public_about) <= 500),
    ADD CONSTRAINT display_name_length
        CHECK (public_display_name IS NULL
               OR char_length(public_display_name) BETWEEN 1 AND 80);

CREATE INDEX idx_orgs_public_enabled
    ON organizations (slug)
    WHERE public_status_enabled = true AND deleted_at IS NULL;

-- Backfill for the canonical default slug. Fresh installs match zero rows
-- (no orgs exist yet); on an upgrade it preserves the visible /status page.
-- Operators who renamed `tenancy.default_org_slug` must flip the flag once
-- by hand — `ensure_default_org` only sets it on the very first INSERT.
UPDATE organizations
   SET public_status_enabled = true
 WHERE slug = 'default' AND deleted_at IS NULL;
