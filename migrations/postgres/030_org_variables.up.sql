-- Reusable named values an org references from monitor request fields as
-- {{key}} literals, resolved at probe time. A secret variable stores a sealed
-- envelope (or plaintext when no KEK), is write-only, and is decrypted only
-- worker-side; the monitor's check_spec never holds the resolved value.
CREATE TABLE org_variables (
    id         UUID PRIMARY KEY DEFAULT uuidv7(),
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    key        TEXT NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
    is_secret  BOOLEAN NOT NULL DEFAULT false,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL
);

-- Unique per org; its org_id prefix also serves every org-scoped lookup.
CREATE UNIQUE INDEX idx_org_variables_org_key ON org_variables (org_id, key);
