-- Lossy: anyone who bought the reshaped `pro` lands on the larger tier.

UPDATE plans SET
    name                        = 'Pro',
    description                 = 'For teams and businesses running production services',
    max_targets                 = 150,
    max_flow_checks             = 0,
    min_check_interval_secs     = 30,
    retention_days              = 395,
    max_members                 = 15,
    max_pending_invitations     = 25,
    max_api_tokens_per_user     = 10,
    max_public_components       = 75,
    max_status_pages            = 5,
    max_share_links_per_monitor = 5,
    max_shared_monitors         = 10,
    max_maintenance_windows     = 50,
    max_notification_channels   = 50,
    max_escalation_policies     = 50,
    max_on_call_schedules       = 25,
    on_call_enabled             = true,
    updated_at                  = now()
WHERE id = 'pro';

UPDATE accounts SET plan_id = 'pro', updated_at = now() WHERE plan_id = 'team';

DELETE FROM plans WHERE id = 'team';

UPDATE plans SET
    max_escalation_policies = 10,
    max_on_call_schedules   = 5,
    on_call_enabled         = true,
    updated_at              = now()
WHERE id IN ('free', 'founding');

UPDATE plans SET max_flow_checks = 0, updated_at = now() WHERE id = 'founding';
