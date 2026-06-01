-- Public status pages: one or more per org (plan-capped). Each owns its
-- branding + a globally-unique subdomain slug and selects its monitors via the
-- status_page_components join; a monitor can sit on several pages with a
-- different public name/group/order on each.

CREATE TABLE status_pages (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                   UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Globally unique (it routes a subdomain), not per-org. CITEXT so case-only
    -- variants collide; the canonical-form CHECK lets routing skip normalising.
    slug                     CITEXT NOT NULL UNIQUE,
    name                     TEXT NOT NULL,
    enabled                  BOOLEAN NOT NULL DEFAULT false,
    public_display_name      TEXT,
    public_about             TEXT,
    public_brand_color       TEXT,
    -- NULL = no override, fall back to config.default_show_powered_by at read
    -- time; a NOT NULL DEFAULT would make that tri-state unrepresentable.
    public_show_powered_by   BOOLEAN,
    public_style             TEXT NOT NULL DEFAULT 'default',
    -- `custom_css` is the raw input;
    -- `custom_css_compiled` is the sanitized form served publicly (so the render
    -- path never re-sanitizes or emits raw); `custom_css_enabled` toggles it.
    custom_css               TEXT,
    custom_css_compiled      TEXT,
    custom_css_enabled       BOOLEAN NOT NULL DEFAULT false,
    write_source             TEXT NOT NULL DEFAULT 'ui'
                             CHECK (write_source IN ('ui', 'api', 'terraform')),
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT status_page_slug_canonical
        CHECK (slug::text = lower(slug::text) AND slug::text NOT LIKE '%.'),
    -- Bound stored CSS so it can't bloat every rendered page.
    CONSTRAINT status_page_custom_css_length
        CHECK (custom_css IS NULL OR char_length(custom_css) <= 20000),
    CONSTRAINT status_page_brand_color_format
        CHECK (public_brand_color IS NULL
               OR public_brand_color ~ '^#[0-9a-fA-F]{6}$'),
    CONSTRAINT status_page_about_length
        CHECK (public_about IS NULL OR char_length(public_about) <= 500),
    CONSTRAINT status_page_display_name_length
        CHECK (public_display_name IS NULL
               OR char_length(public_display_name) BETWEEN 1 AND 80),
    CONSTRAINT status_page_style_known
        CHECK (public_style IN (
            'default', 'classic', 'terminal', 'winter',
            'dark', 'night', 'dim', 'nord', 'dracula',
            'corporate', 'light', 'cupcake', 'cyberpunk', 'synthwave'
        ))
);

CREATE INDEX idx_status_pages_slug_enabled
    ON status_pages (slug) WHERE enabled = true;
CREATE INDEX idx_status_pages_org ON status_pages (org_id);

-- A page's monitors + per-page curation. PK = one binding per (page, target).
-- org_id is denormalised so the org-match triggers and the per-org
-- distinct-target component cap query without joining up to the page.
CREATE TABLE status_page_components (
    org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    status_page_id      UUID NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
    target_id           UUID NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    public_name         TEXT,
    public_description  TEXT,
    public_group        TEXT,
    sort_order          INTEGER NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (status_page_id, target_id),

    CONSTRAINT spc_about_length
        CHECK (public_description IS NULL OR char_length(public_description) <= 200),
    CONSTRAINT spc_public_name_length
        CHECK (public_name IS NULL OR char_length(public_name) BETWEEN 1 AND 80),
    CONSTRAINT spc_public_group_length
        CHECK (public_group IS NULL OR char_length(public_group) <= 50)
);

-- Per-org distinct-target_id count for the public-component cap.
CREATE INDEX idx_spc_org_target ON status_page_components (org_id, target_id);
-- Aggregator render order.
CREATE INDEX idx_spc_page_order
    ON status_page_components (status_page_id, public_group, sort_order);

-- Org-match guards: the binding's org must equal both its
-- parent page's org and its target's org.
CREATE TRIGGER trg_status_page_components_org_match
    BEFORE INSERT OR UPDATE OF status_page_id, org_id, target_id ON status_page_components
    FOR EACH ROW EXECUTE FUNCTION assert_org_matches_parent('status_pages', 'status_page_id');

CREATE TRIGGER trg_status_page_components_target_org
    BEFORE INSERT OR UPDATE OF target_id, org_id ON status_page_components
    FOR EACH ROW EXECUTE FUNCTION assert_target_in_same_org();
