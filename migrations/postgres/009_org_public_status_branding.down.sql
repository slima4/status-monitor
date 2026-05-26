DROP INDEX IF EXISTS idx_orgs_public_enabled;

ALTER TABLE organizations
    DROP CONSTRAINT IF EXISTS public_style_known,
    DROP CONSTRAINT IF EXISTS display_name_length,
    DROP CONSTRAINT IF EXISTS about_length,
    DROP CONSTRAINT IF EXISTS brand_color_format;

ALTER TABLE organizations
    DROP COLUMN IF EXISTS public_custom_domain_verified_at,
    DROP COLUMN IF EXISTS public_custom_domain,
    DROP COLUMN IF EXISTS public_show_powered_by,
    DROP COLUMN IF EXISTS public_style,
    DROP COLUMN IF EXISTS public_logo_path,
    DROP COLUMN IF EXISTS public_brand_color,
    DROP COLUMN IF EXISTS public_about,
    DROP COLUMN IF EXISTS public_display_name,
    DROP COLUMN IF EXISTS public_status_enabled;
