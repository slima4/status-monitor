ALTER TABLE status_page_components
    ADD COLUMN detail_link_enabled BOOLEAN NOT NULL DEFAULT false,
    -- Revoke is soft, so this fires only on a hard delete; the read paths gate
    -- on revoked_at/expires_at.
    ADD COLUMN share_id UUID REFERENCES monitor_shares(id) ON DELETE SET NULL;
