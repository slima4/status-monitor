-- Align the plan catalog with the four tiers the pricing page sells.
-- `plans.id` is immutable by trigger, so `pro` is reshaped rather than renamed.

-- Written as literals so the tier means the same on every install. Deriving
-- these from the live `pro` row would let an operator's local edit redefine a
-- shipped tier, leaving `team` worth 10 monitors here and 1000 there.
INSERT INTO plans (
    id, name, description,
    max_orgs, max_targets, min_check_interval_secs,
    retention_days, raw_days, evidence_days,
    max_members, max_pending_invitations, max_api_tokens_per_user,
    max_public_components, max_status_pages,
    max_share_links_per_monitor, max_shared_monitors,
    max_maintenance_windows, max_notification_channels,
    max_escalation_policies, max_on_call_schedules,
    max_regions, max_flow_steps, max_flow_checks, max_logo_size_bytes,
    api_writes_per_minute, api_reads_per_minute,
    bulk_ops_per_minute, test_now_per_minute, check_now_per_minute,
    custom_domain_enabled, white_label_enabled, sms_alerts_enabled,
    incident_narration_enabled, on_call_enabled, is_listed
)
VALUES (
    'team', 'Team', 'For teams running production services at real scale',
    10, 150, 30,
    395, 30, 7,
    15, 25, 10,
    75, 5,
    5, 10,
    50, 50,
    50, 25,
    2147483647, 30, 10, 1048576,
    1800, 18000,
    90, 180, 180,
    true, true, true,
    true, true, false
);

UPDATE plans SET
    name                        = 'Pro',
    description                 = 'For a product that needs the status page on its own domain',
    max_targets                 = 50,
    max_flow_checks             = 3,
    min_check_interval_secs     = 60,
    retention_days              = 90,
    max_members                 = 5,
    max_pending_invitations     = 15,
    max_api_tokens_per_user     = 7,
    max_public_components       = 30,
    max_status_pages            = 2,
    max_share_links_per_monitor = 3,
    max_shared_monitors         = 5,
    max_maintenance_windows     = 30,
    max_notification_channels   = 30,
    max_escalation_policies     = 0,
    max_on_call_schedules       = 0,
    on_call_enabled             = false,
    updated_at                  = now()
WHERE id = 'pro';

-- The column ships 0 as a gate; founding is sold with one browser flow.
UPDATE plans SET max_flow_checks = 1, updated_at = now() WHERE id = 'founding';

-- On-call and escalation become a Team feature.
UPDATE plans SET
    max_escalation_policies = 0,
    max_on_call_schedules   = 0,
    on_call_enabled         = false,
    updated_at              = now()
WHERE id IN ('free', 'founding');
