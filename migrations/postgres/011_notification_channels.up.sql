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
    -- Closed enum; keep in lockstep with `domain::ChannelKind::ALL`. The
    -- live drift test (`tests/enum_drift_test.rs`) introspects this CHECK
    -- and fails if the lists disagree.
    kind        TEXT NOT NULL CHECK (kind IN ('webhook', 'slack', 'telegram', 'telegram_app', 'whatsapp', 'whatsapp_app', 'discord', 'msteams', 'google_chat', 'email', 'pagerduty', 'ntfy', 'pushover')),
    config      JSONB NOT NULL,
    -- Routing id (e.g. telegram chat id) queryable without opening the
    -- sealed config; set only by the transport's own flow.
    external_ref TEXT,
    enabled     BOOLEAN NOT NULL DEFAULT true,
    -- Platform-disable note shown in the UI; cleared on re-enable.
    disabled_reason TEXT,
    -- Email delivery gate: set when the address confirms its verification
    -- link, reset on config change; NULL for every other kind.
    verified_at TIMESTAMPTZ,
    write_source TEXT NOT NULL DEFAULT 'ui'
                CHECK (write_source IN ('ui', 'api', 'terraform')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Names are the human handle in the target-binding UI; unique per tenant.
    UNIQUE (org_id, name)
);

CREATE INDEX idx_notification_channels_org ON notification_channels(org_id);

CREATE INDEX idx_notification_channels_external_ref
    ON notification_channels (kind, external_ref)
    WHERE external_ref IS NOT NULL;
