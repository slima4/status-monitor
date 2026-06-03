-- Durable audit trail for MCP write tools. Every confirmed mutation (and every
-- denied attempt) writes a row here, in addition to a structured tracing event.
-- Reads are never audited — only state-changing tools.
CREATE TABLE mcp_audit (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_type  TEXT NOT NULL DEFAULT 'mcp',
    -- The api_tokens row the call authenticated with. No FK: the token may be
    -- rotated/revoked later, but its audit history must survive.
    token_id    UUID,
    -- The human behind the token / the org acted on. Keep the row if the user
    -- is later deleted (audit outlives the actor); drop with the tenant.
    user_id     UUID REFERENCES users(id) ON DELETE SET NULL,
    org_id      UUID REFERENCES organizations(id) ON DELETE CASCADE,
    tool        TEXT NOT NULL,
    arguments   JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- 'success' | 'error' | 'denied'
    outcome     TEXT NOT NULL,
    -- Safe extra context (denial reason, sanitized error). Never secrets.
    detail      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Per-org activity view (newest first) + per-token forensics.
CREATE INDEX idx_mcp_audit_org   ON mcp_audit (org_id, created_at DESC);
CREATE INDEX idx_mcp_audit_token ON mcp_audit (token_id);
