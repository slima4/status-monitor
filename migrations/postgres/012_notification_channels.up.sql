-- Per-org notification channels (Slack / generic webhook / Telegram / …).
-- A target binds to one or more of these for Down/Recovered alerts.
--
-- `config` holds the transport secrets. It is sealed by the credentials KEK
-- as {"$enc":"v1:…"} when one is configured, or stored as plaintext JSON for
-- the no-KEK self-host case — the exact at-rest convention already used for
-- targets.check_spec credentials.
CREATE TABLE notification_channels (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    config      JSONB NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Names are the human handle in the target-binding UI; unique per tenant.
    UNIQUE (org_id, name)
);

CREATE INDEX idx_notification_channels_org ON notification_channels(org_id);
