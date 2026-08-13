use super::*;
use std::collections::HashMap;
use std::time::Duration as StdDuration;

use crate::domain::NotificationStatus;
use crate::domain::{
    AlertBinding, ChannelConfig, CheckSpec, EscalationTargetType, ExpectedStatus, HttpCheck,
    HttpMethod, IncidentOrigin, IncidentSeverity, IncidentState, IncidentUrgency,
    IncidentVisibility, NewEscalationPolicy, NewEscalationStep, NewEscalationTarget,
    NewNotificationChannel, OpsIncident, Target, TargetAlerts, WebhookConfig, WriteSource,
};
use crate::storage::{
    Actor, DueIncident, InMemoryContactStore, InMemoryEscalationPolicyStore,
    InMemoryIncidentOpsStore, InMemoryNotificationChannelStore, InMemoryOnCallStore,
    InMemoryTargetStore,
};

use super::rules::{log_error_snippet, redact_secrets, retry_after_hint, retry_delay_secs};

fn org() -> OrgId {
    OrgId(Uuid::nil())
}

// A channel whose transport always fails fast (closed loopback port), so a
// delivery attempt records `failed` without needing a mock HTTP server.
async fn failing_channel(store: &InMemoryNotificationChannelStore) -> Uuid {
    store
        .create(
            org(),
            NewNotificationChannel {
                name: format!("ops-{}", Uuid::now_v7()),
                config: ChannelConfig::Webhook(WebhookConfig {
                    url: "http://127.0.0.1:1/notify".into(),
                    headers: Default::default(),
                    secret: None,
                }),
                enabled: true,
            },
            WriteSource::Ui,
            100,
            None,
        )
        .await
        .unwrap()
        .id
}

fn target_with_channel(channel_id: Uuid) -> Target {
    target_with_channel_recovery(channel_id, true)
}

fn target_with_channel_recovery(channel_id: Uuid, notify_recovery: bool) -> Target {
    Target {
        id: Uuid::now_v7(),
        name: "api".into(),
        check: CheckSpec::Http(HttpCheck {
            url: url::Url::parse("https://example.com/").unwrap(),
            method: HttpMethod::Get,
            timeout: StdDuration::from_secs(5),
            follow_redirects: false,
            max_redirects: 0,
            expected_status: ExpectedStatus::Exact(200),
            expected_body_contains: None,
            headers: HashMap::new(),
            body: None,
            verify_tls: true,
            basic_auth: None,
            bearer_token: None,
        }),
        interval: StdDuration::from_secs(30),
        enabled: true,
        tags: vec![],
        alerts: TargetAlerts(vec![AlertBinding { channel_id }]),
        alert_confirmations: 1,
        notify_recovery,
        renotify_interval_secs: 3600,
        region_policy: Default::default(),
        group_name: None,
        owner_user_id: None,
        write_source: WriteSource::Ui,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn bare_target() -> Target {
    let mut t = target_with_channel(Uuid::now_v7());
    t.alerts = TargetAlerts(vec![]);
    t
}

fn seed_incident(ops: &InMemoryIncidentOpsStore, target_id: Option<Uuid>) -> Uuid {
    let now = Utc::now();
    let id = Uuid::now_v7();
    ops.seed(OpsIncident {
        id,
        target_id,
        title: None,
        state: IncidentState::Triggered,
        severity: IncidentSeverity::Major,
        urgency: IncidentUrgency::High,
        origin: IncidentOrigin::Monitor,
        visibility: IncidentVisibility::Internal,
        started_at: now,
        ended_at: None,
        acknowledged_at: None,
        acknowledged_by: None,
        assigned_to: None,
        resolved_by: None,
        escalation_policy_id: None,
        escalation_level: 0,
        escalation_round: 0,
        next_escalation_at: None,
        check_count: 2,
        error_sample: Some("boom".into()),
        regions_down: Vec::new(),
        regions_up: Vec::new(),
        created_at: now,
        updated_at: now,
    });
    id
}

fn channel_step(level: i32, delay: i32, channel: Uuid) -> NewEscalationStep {
    NewEscalationStep {
        level,
        delay_secs: delay,
        targets: vec![NewEscalationTarget {
            target_type: EscalationTargetType::Channel,
            user_id: None,
            schedule_id: None,
            channel_id: Some(channel),
        }],
    }
}

fn engine(
    ops: Arc<dyn IncidentOpsStore>,
    policies: Arc<dyn EscalationPolicyStore>,
    targets: Arc<dyn TargetStore>,
    channels: Arc<dyn NotificationChannelStore>,
) -> EscalationEngine {
    engine_with(
        ops,
        policies,
        Arc::new(InMemoryOnCallStore::new()),
        Arc::new(InMemoryContactStore::new()),
        targets,
        channels,
    )
}

fn engine_with(
    ops: Arc<dyn IncidentOpsStore>,
    policies: Arc<dyn EscalationPolicyStore>,
    on_call: Arc<dyn OnCallStore>,
    contacts: Arc<dyn ContactStore>,
    targets: Arc<dyn TargetStore>,
    channels: Arc<dyn NotificationChannelStore>,
) -> EscalationEngine {
    let (_tx, rx) = mpsc::channel(4);
    EscalationEngine::new(
        rx,
        EngineDeps {
            ops,
            policies,
            on_call,
            contacts,
            targets,
            channels,
            orgs: Arc::new(crate::storage::orgs::InMemoryOrgDirectory::new()),
            http: crate::http_outbound::build_outbound_client(
                crate::security::SsrfGuard::relaxed_for_tests(),
            ),
            cfg: EscalationConfig::default(),
            base_url: String::new(),
            alert_channel_stop_secret: String::new(),
            central_bot: None,
            central_whatsapp: None,
            email: None,
        },
    )
}

#[tokio::test]
async fn no_policy_falls_back_to_bound_channels_once() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
    // A duplicate Opened signal does not re-page the same episode.
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn unverified_email_channel_records_failure_without_sending() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = channels
        .create(
            org(),
            NewNotificationChannel {
                name: "oncall-mail".into(),
                config: ChannelConfig::Email(crate::domain::EmailConfig {
                    to: "oncall@example.com".into(),
                }),
                enabled: true,
            },
            WriteSource::Ui,
            100,
            None,
        )
        .await
        .unwrap()
        .id;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels.clone());
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    let rows = ops.notifications_for(org(), id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, NotificationStatus::Failed);
    assert_eq!(rows[0].error.as_deref(), Some("email address not verified"));

    // Once verified, delivery is attempted for real (and fails on the
    // missing email sender — a different error than the gate's).
    let upd = channels.get(org(), cid).await.unwrap().unwrap().updated_at;
    assert!(channels.set_verified(org(), cid, upd).await.unwrap());
    let id2 = seed_incident(&ops, Some(tid));
    eng.page(org(), id2, NotificationReason::Opened)
        .await
        .unwrap();
    let rows = ops.notifications_for(org(), id2).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, NotificationStatus::Failed);
    assert_ne!(rows[0].error.as_deref(), Some("email address not verified"));
}

#[tokio::test]
async fn policy_pages_first_level_and_arms_the_timer() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let c1 = failing_channel(&channels).await;
    let target = bare_target();
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());
    let p = policies
        .create(
            org(),
            NewEscalationPolicy {
                name: "p".into(),
                description: None,
                repeat_count: 0,
                steps: vec![channel_step(1, 300, c1)],
            },
            10,
        )
        .await
        .unwrap();
    policies
        .set_target_policy(org(), tid, Some(p.id))
        .await
        .unwrap();

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    let rows = ops.notifications_for(org(), id).await.unwrap();
    assert_eq!(rows.len(), 1);
    let inc = ops.get(org(), id).await.unwrap().unwrap();
    assert_eq!(inc.escalation_level, 1);
    assert_eq!(inc.escalation_policy_id, Some(p.id));
    assert!(
        inc.next_escalation_at.is_some(),
        "timer is armed for level 2"
    );
}

#[tokio::test]
async fn sweep_walks_to_the_next_level_then_exhausts() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let c1 = failing_channel(&channels).await;
    let c2 = failing_channel(&channels).await;
    let target = bare_target();
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());
    let p = policies
        .create(
            org(),
            NewEscalationPolicy {
                name: "p".into(),
                description: None,
                repeat_count: 0,
                steps: vec![channel_step(1, 0, c1), channel_step(2, 0, c2)],
            },
            10,
        )
        .await
        .unwrap();
    policies
        .set_target_policy(org(), tid, Some(p.id))
        .await
        .unwrap();

    let eng = engine(ops.clone(), policies, targets, channels);
    // Level 1 page + arm (delay 0 → immediately due).
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    assert_eq!(
        ops.get(org(), id).await.unwrap().unwrap().escalation_level,
        1
    );

    // Sweep escalates to level 2.
    eng.escalate_due().await;
    let inc = ops.get(org(), id).await.unwrap().unwrap();
    assert_eq!(inc.escalation_level, 2);
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 2);

    // Next sweep: last level, no repeat → exhausted, timer disarmed.
    eng.escalate_due().await;
    let inc = ops.get(org(), id).await.unwrap().unwrap();
    assert!(inc.next_escalation_at.is_none());
    // No further pages once exhausted.
    eng.escalate_due().await;
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn acknowledge_halts_the_sweep() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let c1 = failing_channel(&channels).await;
    let c2 = failing_channel(&channels).await;
    let target = bare_target();
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());
    let p = policies
        .create(
            org(),
            NewEscalationPolicy {
                name: "p".into(),
                description: None,
                repeat_count: 0,
                steps: vec![channel_step(1, 0, c1), channel_step(2, 0, c2)],
            },
            10,
        )
        .await
        .unwrap();
    policies
        .set_target_policy(org(), tid, Some(p.id))
        .await
        .unwrap();

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    // A responder acks → next_escalation_at cleared, state acknowledged.
    ops.acknowledge(org(), id, Actor::System, None)
        .await
        .unwrap();
    eng.escalate_due().await;
    // Still only the level-1 page; the sweep found nothing due.
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn resolution_notifies_paged_channels_and_dedups() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    ops.resolve(org(), id, Actor::System, None).await.unwrap();
    eng.page(org(), id, NotificationReason::Resolved)
        .await
        .unwrap();
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 2);
    // A duplicate Resolved signal is absorbed.
    eng.page(org(), id, NotificationReason::Resolved)
        .await
        .unwrap();
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn notify_recovery_false_suppresses_the_resolved_page() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel_recovery(cid, false);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    ops.resolve(org(), id, Actor::System, None).await.unwrap();
    eng.page(org(), id, NotificationReason::Resolved)
        .await
        .unwrap();
    // Opened paged once; recovery opt-out blocked the resolution page.
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn renotify_re_pages_an_open_unacked_incident_then_stops_once_acked() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);

    let due = DueIncident {
        id,
        org: org(),
        target_id: Some(tid),
        escalation_policy_id: None,
        escalation_level: 0,
        escalation_round: 0,
    };
    eng.renotify_one(&due).await.unwrap();
    assert_eq!(
        ops.notifications_for(org(), id).await.unwrap().len(),
        2,
        "reminder re-pages the bound channel while down + unacked"
    );

    ops.acknowledge(org(), id, Actor::System, None)
        .await
        .unwrap();
    eng.renotify_one(&due).await.unwrap();
    assert_eq!(
        ops.notifications_for(org(), id).await.unwrap().len(),
        2,
        "an acknowledged incident is not reminded"
    );
}

#[tokio::test]
async fn schedule_target_pages_the_on_call_responders_contact_channel() {
    use crate::domain::{
        NewOnCallLayer, NewOnCallParticipant, NewOnCallSchedule, RotationType, UserId,
    };

    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let personal = failing_channel(&channels).await;
    let responder = UserId(Uuid::now_v7());

    // On-call schedule with the responder, and their personal contact channel.
    let on_call = Arc::new(InMemoryOnCallStore::new());
    on_call.add_member(org(), responder);
    let sched = on_call
        .create(
            org(),
            NewOnCallSchedule {
                name: "primary".into(),
                timezone: "UTC".into(),
                layers: vec![NewOnCallLayer {
                    name: None,
                    rotation_type: RotationType::Daily,
                    rotation_length_secs: 86_400,
                    handoff_at: "2020-01-01T00:00:00Z".parse().unwrap(),
                    layer_order: 0,
                    participants: vec![NewOnCallParticipant { user_id: responder }],
                }],
            },
            10,
        )
        .await
        .unwrap();
    let contacts = Arc::new(InMemoryContactStore::new());
    contacts.add_channel(org(), personal);
    contacts
        .replace_for_user(org(), responder, vec![personal])
        .await
        .unwrap();

    // A policy whose only rung is a schedule target.
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());
    let p = policies
        .create(
            org(),
            crate::domain::NewEscalationPolicy {
                name: "p".into(),
                description: None,
                repeat_count: 0,
                steps: vec![NewEscalationStep {
                    level: 1,
                    delay_secs: 300,
                    targets: vec![NewEscalationTarget {
                        target_type: EscalationTargetType::Schedule,
                        user_id: None,
                        schedule_id: Some(sched.schedule.id),
                        channel_id: None,
                    }],
                }],
            },
            10,
        )
        .await
        .unwrap();
    let target = bare_target();
    let tid = target.id;
    policies
        .set_target_policy(org(), tid, Some(p.id))
        .await
        .unwrap();
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));

    let eng = engine_with(ops.clone(), policies, on_call, contacts, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    let rows = ops.notifications_for(org(), id).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the on-call responder's contact channel was paged"
    );
    assert_eq!(rows[0].channel_id, Some(personal));
    assert_eq!(rows[0].target_user_id, Some(responder));
}

#[tokio::test]
async fn reconcile_pages_an_incident_whose_open_signal_was_dropped() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    // A triggered incident older than the grace window, never paged (its
    // Opened signal was dropped): no notifications, no policy, no timer.
    let now = Utc::now();
    let id = Uuid::now_v7();
    ops.seed(OpsIncident {
        id,
        target_id: Some(tid),
        title: None,
        state: IncidentState::Triggered,
        severity: IncidentSeverity::Major,
        urgency: IncidentUrgency::High,
        origin: IncidentOrigin::Monitor,
        visibility: IncidentVisibility::Internal,
        started_at: now - chrono::Duration::seconds(120),
        ended_at: None,
        acknowledged_at: None,
        acknowledged_by: None,
        assigned_to: None,
        resolved_by: None,
        escalation_policy_id: None,
        escalation_level: 0,
        escalation_round: 0,
        next_escalation_at: None,
        check_count: 2,
        error_sample: None,
        regions_down: Vec::new(),
        regions_up: Vec::new(),
        created_at: now - chrono::Duration::seconds(120),
        updated_at: now - chrono::Duration::seconds(120),
    });
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    assert!(ops.notifications_for(org(), id).await.unwrap().is_empty());
    eng.reconcile().await;
    assert_eq!(
        ops.notifications_for(org(), id).await.unwrap().len(),
        1,
        "reconcile re-pages the never-paged incident"
    );
    // Now that it has been paged, reconcile no longer picks it up.
    eng.reconcile().await;
    assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn reconcile_leaves_an_incident_older_than_its_window_alone() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let target = target_with_channel(Uuid::now_v7());
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let now = Utc::now();
    let id = Uuid::now_v7();
    let old = now - chrono::Duration::days(21);
    ops.seed(OpsIncident {
        id,
        target_id: Some(tid),
        title: None,
        state: IncidentState::Triggered,
        severity: IncidentSeverity::Major,
        urgency: IncidentUrgency::High,
        origin: IncidentOrigin::Monitor,
        visibility: IncidentVisibility::Internal,
        started_at: old,
        ended_at: None,
        acknowledged_at: None,
        acknowledged_by: None,
        assigned_to: None,
        resolved_by: None,
        escalation_policy_id: None,
        escalation_level: 0,
        escalation_round: 0,
        next_escalation_at: None,
        check_count: 2,
        error_sample: None,
        regions_down: Vec::new(),
        regions_up: Vec::new(),
        created_at: old,
        updated_at: old,
    });
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.reconcile().await;
    assert!(
        ops.notifications_for(org(), id).await.unwrap().is_empty(),
        "an incident past the reconcile window is not re-attempted"
    );
}

#[test]
fn retry_backoff_doubles_then_dead_letters() {
    // base 30s, cap 1h, 5 attempts: 30, 60, 120, 240, then None (exhausted).
    assert_eq!(retry_delay_secs(1, 30, 3600, 5), Some(30));
    assert_eq!(retry_delay_secs(2, 30, 3600, 5), Some(60));
    assert_eq!(retry_delay_secs(3, 30, 3600, 5), Some(120));
    assert_eq!(retry_delay_secs(4, 30, 3600, 5), Some(240));
    assert_eq!(
        retry_delay_secs(5, 30, 3600, 5),
        None,
        "attempt cap → dead-letter"
    );
    // The doubling is bounded by the configured cap.
    assert_eq!(retry_delay_secs(10, 30, 3600, 100), Some(3600));
    assert_eq!(retry_delay_secs(4, 30, 90, 100), Some(90), "cap bites");
}

#[test]
fn retry_after_hint_reads_telegram_429_body() {
    let body = r#"sending request failed: 429 Too Many Requests: {"ok":false,"error_code":429,"description":"Too Many Requests: retry after 31","parameters":{"retry_after":31}}"#;
    assert_eq!(
        retry_after_hint(Some(body)),
        Some(chrono::Duration::seconds(31))
    );
    assert_eq!(retry_after_hint(Some("connection refused")), None);
    assert_eq!(retry_after_hint(None), None);
    // A hostile/buggy body can't park the retry for days.
    assert_eq!(
        retry_after_hint(Some(r#"{"retry_after":9999999}"#)),
        Some(chrono::Duration::seconds(3600))
    );
    assert_eq!(retry_after_hint(Some(r#"{"retry_after":"x"}"#)), None);
}

#[test]
fn retry_after_hint_survives_redaction() {
    // The hint is parsed AFTER redact_secrets; this pins that the
    // URL-token redaction never eats the JSON fragment.
    let raw = r#"https://api.telegram.org/bot123:SECRET/sendMessage returned 429 Too Many Requests: {"ok":false,"error_code":429,"parameters":{"retry_after":31}}"#;
    let redacted = redact_secrets(raw);
    assert_eq!(
        retry_after_hint(Some(&redacted)),
        Some(chrono::Duration::seconds(31))
    );
}

#[test]
fn redact_secrets_strips_channel_url_paths() {
    let slack = "POST https://hooks.slack.com/services/T01/B02/abcSECRETxyz failed: 404";
    let out = redact_secrets(slack);
    assert!(out.contains("https://hooks.slack.com"));
    assert!(
        !out.contains("abcSECRETxyz"),
        "the webhook secret must not survive"
    );
    assert!(!out.contains("/services/"));

    let tg = "https://api.telegram.org/bot123456:AAH-SECRET-TOKEN/sendMessage 401";
    let out = redact_secrets(tg);
    assert!(out.contains("https://api.telegram.org"));
    assert!(
        !out.contains("SECRET-TOKEN"),
        "the bot token must not survive"
    );

    // Non-URL text is untouched.
    assert_eq!(redact_secrets("connection refused"), "connection refused");

    // A "://"-bearing token that does not cleanly parse is dropped wholesale
    // rather than echoed (it might still carry the secret path).
    let bad = redact_secrets("weird://[bad/SECRET-path");
    assert!(
        !bad.contains("SECRET-path"),
        "an unparseable url token must not survive"
    );
    assert!(bad.contains("[redacted-url]"));
}

#[test]
fn log_error_snippet_masks_addresses_and_clips() {
    let out = log_error_snippet("delivery rejected for <oncall@example.com>: mailbox full");
    assert!(!out.contains("oncall@example.com"));
    assert!(out.contains("[redacted-address]"));
    assert!(out.contains("mailbox full"));

    // A telegram-style @handle has no local part and stays readable.
    assert_eq!(
        log_error_snippet("chat @ops_team not found"),
        "chat @ops_team not found"
    );

    let big = format!("endpoint returned 503: {}", "x".repeat(10_000));
    let out = log_error_snippet(&big);
    assert!(
        out.chars().count() <= 257,
        "clipped to the cap plus ellipsis"
    );
    assert!(out.ends_with('…'));
}

#[tokio::test]
async fn manual_incident_without_monitor_pages_nothing() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, None);
    let targets = Arc::new(InMemoryTargetStore::new());
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());
    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    assert!(ops.notifications_for(org(), id).await.unwrap().is_empty());
}

#[tokio::test]
async fn retry_sweep_increments_attempts_then_exhausts() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    for _ in 0..10 {
        // Each round simulates the exponential backoff having elapsed.
        ops.clear_retry_backoff();
        eng.retry_pending().await;
    }
    let rows = ops.notifications_for(org(), id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].attempt,
        EscalationConfig::default().max_attempts as i32
    );
    assert_eq!(rows[0].status, NotificationStatus::Failed);
}

#[tokio::test]
async fn retry_drops_a_page_whose_reason_no_longer_matches_state() {
    let channels = Arc::new(InMemoryNotificationChannelStore::new());
    let cid = failing_channel(&channels).await;
    let target = target_with_channel(cid);
    let tid = target.id;
    let ops = Arc::new(InMemoryIncidentOpsStore::new());
    let id = seed_incident(&ops, Some(tid));
    let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
    let policies = Arc::new(InMemoryEscalationPolicyStore::new());

    let eng = engine(ops.clone(), policies, targets, channels);
    eng.page(org(), id, NotificationReason::Opened)
        .await
        .unwrap();
    ops.resolve(org(), id, Actor::System, None).await.unwrap();
    ops.clear_retry_backoff();
    eng.retry_pending().await;
    let rows = ops.notifications_for(org(), id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, NotificationStatus::Suppressed);
}
