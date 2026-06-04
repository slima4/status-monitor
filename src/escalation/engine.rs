use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::EscalationConfig;
use crate::domain::{
    EscalationDecision, EscalationPolicy, EscalationTargetType, IncidentEventKind, IncidentState,
    NewIncidentNotification, NotificationReason, NotificationStatus, OpsIncident, OrgId, Target,
    next_step,
};
use crate::error::Result;
use crate::http_outbound::OutboundHttpClient;
use crate::notifier::build_notifier;
use crate::notifier::event::IncidentNotice;
use crate::storage::{
    Actor, DueIncident, EscalationPolicyStore, IncidentOpsStore, NotificationChannelStore,
    PendingNotification, TargetStore,
};

/// A nudge that an incident's state changed and its paging should be
/// reconciled. Carries no payload beyond identity + the reason to page; the
/// engine re-reads the incident, monitor, and channels so a stale signal never
/// pages outdated content.
pub struct IncidentSignal {
    pub org: OrgId,
    pub incident_id: Uuid,
    pub reason: NotificationReason,
}

pub struct EscalationEngine {
    rx: mpsc::Receiver<IncidentSignal>,
    ops: Arc<dyn IncidentOpsStore>,
    policies: Arc<dyn EscalationPolicyStore>,
    targets: Arc<dyn TargetStore>,
    channels: Arc<dyn NotificationChannelStore>,
    http: OutboundHttpClient,
    cfg: EscalationConfig,
    /// Operator base URL for incident deep links; empty omits the link.
    base_url: String,
}

impl EscalationEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rx: mpsc::Receiver<IncidentSignal>,
        ops: Arc<dyn IncidentOpsStore>,
        policies: Arc<dyn EscalationPolicyStore>,
        targets: Arc<dyn TargetStore>,
        channels: Arc<dyn NotificationChannelStore>,
        http: OutboundHttpClient,
        cfg: EscalationConfig,
        base_url: String,
    ) -> Self {
        Self {
            rx,
            ops,
            policies,
            targets,
            channels,
            http,
            cfg,
            base_url,
        }
    }

    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut tick =
            tokio::time::interval(Duration::from_secs(self.cfg.tick_interval_secs.max(1)));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                maybe = self.rx.recv() => match maybe {
                    Some(sig) => {
                        if let Err(err) = self.page(sig.org, sig.incident_id, sig.reason).await {
                            tracing::warn!(incident_id = %sig.incident_id, error = %err, "incident paging failed");
                        }
                    }
                    None => return,
                },
                _ = tick.tick() => {
                    self.escalate_due().await;
                    self.retry_pending().await;
                }
            }
        }
    }

    /// Handle a lifecycle signal. Opened/Reopened start the escalation episode
    /// (page the first rung, arm the timer); Resolved notifies the channels
    /// already paged this episode. The escalation sweep handles later rungs.
    async fn page(&self, org: OrgId, incident_id: Uuid, reason: NotificationReason) -> Result<()> {
        let Some(incident) = self.ops.get(org, incident_id).await? else {
            return Ok(());
        };
        let Some(target_id) = incident.target_id else {
            return Ok(());
        };
        let Some(target) = self.targets.get(org, target_id).await? else {
            return Ok(());
        };
        match reason {
            NotificationReason::Opened | NotificationReason::Reopened => {
                self.open_episode(org, &incident, &target, reason).await
            }
            NotificationReason::Resolved => self.notify_resolution(org, &incident, &target).await,
            // Escalation pages originate from the sweep, never an inbound signal.
            NotificationReason::Escalated => Ok(()),
        }
    }

    /// Page the first rung and arm escalation. A duplicate Opened signal while
    /// the episode is already paged is a no-op; a monitor with no policy falls
    /// back to its bound channels (the pre-policy behaviour) with no laddered
    /// re-paging.
    async fn open_episode(
        &self,
        org: OrgId,
        incident: &OpsIncident,
        target: &Target,
        reason: NotificationReason,
    ) -> Result<()> {
        let already = self.ops.notifications_for(org, incident.id).await?;
        if open_episode_active(&already) {
            return Ok(());
        }
        let notice = self.notice(incident, target, reason);
        match self.policies.resolve_for_target(org, target.id).await? {
            Some(policy_id) => {
                let Some(policy) = self.policies.get(org, policy_id).await? else {
                    return Ok(());
                };
                match next_step(&policy.steps, policy.repeat_count, 0, 0) {
                    EscalationDecision::Page {
                        level, delay_secs, ..
                    } => {
                        let channels = channels_at_level(&policy, level);
                        let paged = self
                            .page_channels(org, incident.id, &notice, reason, level, &channels)
                            .await?;
                        let next_at = Some(Utc::now() + chrono::Duration::seconds(delay_secs.into()));
                        self.ops
                            .begin_escalation(org, incident.id, policy_id, level, next_at)
                            .await?;
                        self.log_paged(org, incident.id, reason, paged).await?;
                    }
                    EscalationDecision::Exhausted => {
                        // Policy with no steps: record the binding so the
                        // console shows it, but page no one.
                        self.ops
                            .begin_escalation(org, incident.id, policy_id, 0, None)
                            .await?;
                    }
                }
            }
            None => {
                let channels = binding_channels(target);
                let paged = self
                    .page_channels(org, incident.id, &notice, reason, 0, &channels)
                    .await?;
                self.log_paged(org, incident.id, reason, paged).await?;
            }
        }
        Ok(())
    }

    /// Send the all-clear to every channel paged this episode that has not
    /// already had one, honouring a binding's recovery opt-out.
    async fn notify_resolution(
        &self,
        org: OrgId,
        incident: &OpsIncident,
        target: &Target,
    ) -> Result<()> {
        let rows = self.ops.notifications_for(org, incident.id).await?;
        let opted_out = recovery_opted_out(target);
        let channels: Vec<Uuid> = resolvable_channels(&rows)
            .into_iter()
            .filter(|cid| !opted_out.contains(cid))
            .collect();
        if channels.is_empty() {
            return Ok(());
        }
        let notice = self.notice(incident, target, NotificationReason::Resolved);
        let paged = self
            .page_channels(
                org,
                incident.id,
                &notice,
                NotificationReason::Resolved,
                incident.escalation_level,
                &channels,
            )
            .await?;
        self.log_paged(org, incident.id, NotificationReason::Resolved, paged)
            .await
    }

    /// Walk the next rung of every due incident's policy.
    async fn escalate_due(&self) {
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let due = match self.ops.due_for_escalation(Utc::now(), limit).await {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "escalation sweep scan failed");
                return;
            }
        };
        for d in due {
            if let Err(err) = self.escalate_one(&d).await {
                tracing::warn!(incident_id = %d.id, error = %err, "escalation step failed");
            }
        }
    }

    async fn escalate_one(&self, d: &DueIncident) -> Result<()> {
        let Some(policy_id) = d.escalation_policy_id else {
            // Timer armed without a policy — disarm so it does not spin.
            self.ops
                .record_escalation(d.org, d.id, d.escalation_level, d.escalation_round, None)
                .await?;
            return Ok(());
        };
        // A deleted policy stops escalation cleanly.
        let Some(policy) = self.policies.get(d.org, policy_id).await? else {
            self.ops
                .record_escalation(d.org, d.id, d.escalation_level, d.escalation_round, None)
                .await?;
            return Ok(());
        };
        match next_step(
            &policy.steps,
            policy.repeat_count,
            d.escalation_level,
            d.escalation_round,
        ) {
            EscalationDecision::Page {
                level,
                round,
                delay_secs,
            } => {
                let Some(incident) = self.ops.get(d.org, d.id).await? else {
                    return Ok(());
                };
                let Some(target_id) = incident.target_id else {
                    return Ok(());
                };
                let Some(target) = self.targets.get(d.org, target_id).await? else {
                    return Ok(());
                };
                let notice = self.notice(&incident, &target, NotificationReason::Escalated);
                let channels = channels_at_level(&policy, level);
                let paged = self
                    .page_channels(
                        d.org,
                        d.id,
                        &notice,
                        NotificationReason::Escalated,
                        level,
                        &channels,
                    )
                    .await?;
                let next_at = Some(Utc::now() + chrono::Duration::seconds(delay_secs.into()));
                self.ops
                    .record_escalation(d.org, d.id, level, round, next_at)
                    .await?;
                self.log_paged(d.org, d.id, NotificationReason::Escalated, paged)
                    .await?;
            }
            EscalationDecision::Exhausted => {
                self.ops
                    .record_escalation(d.org, d.id, d.escalation_level, d.escalation_round, None)
                    .await?;
                self.ops
                    .append_event(
                        d.org,
                        d.id,
                        IncidentEventKind::Escalated,
                        Actor::System,
                        Some("escalation policy exhausted".into()),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Deliver `reason` to each channel id, recording a `queued` row before the
    /// send so a crash never leaves a delivered page with no audit row. The
    /// caller pre-filters the channel set (dedup, recovery opt-out), so this
    /// pages every id it is handed. Returns how many delivered.
    async fn page_channels(
        &self,
        org: OrgId,
        incident_id: Uuid,
        notice: &IncidentNotice,
        reason: NotificationReason,
        level: i32,
        channels: &[Uuid],
    ) -> Result<u32> {
        let mut paged = 0u32;
        for &cid in channels {
            let Some(channel) = self.channels.get(org, cid).await? else {
                continue;
            };
            if !channel.enabled {
                continue;
            }
            let id = self
                .ops
                .record_notification(NewIncidentNotification {
                    org,
                    incident_id,
                    escalation_level: Some(level),
                    target_user_id: None,
                    channel_id: Some(cid),
                    transport: channel.kind.as_db_str().to_string(),
                    reason,
                    status: NotificationStatus::Queued,
                    attempt: 1,
                    error: None,
                    sent_at: None,
                })
                .await?;
            let (status, error) = self.deliver(&channel.config, notice).await;
            if status == NotificationStatus::Sent {
                paged += 1;
            }
            self.ops
                .mark_notification(
                    org,
                    id,
                    status,
                    1,
                    error,
                    (status == NotificationStatus::Sent).then(Utc::now),
                )
                .await?;
        }
        Ok(paged)
    }

    async fn log_paged(
        &self,
        org: OrgId,
        incident_id: Uuid,
        reason: NotificationReason,
        paged: u32,
    ) -> Result<()> {
        if paged == 0 {
            return Ok(());
        }
        let kind = match reason {
            NotificationReason::Escalated => IncidentEventKind::Escalated,
            _ => IncidentEventKind::Notified,
        };
        self.ops
            .append_event(
                org,
                incident_id,
                kind,
                Actor::System,
                Some(format!("paged {paged} channel(s): {}", reason.as_db_str())),
            )
            .await
    }

    /// Re-attempt failed deliveries under the attempt cap. Each retry updates
    /// the existing row rather than inserting a new one.
    async fn retry_pending(&self) {
        let max_attempts = self.cfg.max_attempts as i32;
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let pending = match self.ops.pending_notifications(limit, max_attempts).await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(error = %err, "escalation retry scan failed");
                return;
            }
        };
        for p in pending {
            if let Err(err) = self.retry_one(&p).await {
                tracing::warn!(notification_id = %p.id, error = %err, "incident page retry failed");
            }
        }
    }

    async fn retry_one(&self, p: &PendingNotification) -> Result<()> {
        let next_attempt = p.attempt + 1;
        // Resolve the same channel the original page targeted. A missing
        // channel/monitor/incident — or a page whose reason no longer matches
        // the incident's state (e.g. an Opened page after the incident already
        // resolved) — is dropped so on-call never gets a stale notice. Burning
        // an attempt lets the row exhaust instead of retrying forever.
        let rebuilt = match p.channel_id {
            Some(cid) => self.rebuild_notice(p.org, p.incident_id, cid, p.reason).await?,
            None => None,
        };
        let Some((notice, channel_cfg, state)) = rebuilt else {
            self.ops
                .mark_notification(p.org, p.id, NotificationStatus::Failed, next_attempt, None, None)
                .await?;
            return Ok(());
        };
        if reason_is_stale(p.reason, state) {
            self.ops
                .mark_notification(
                    p.org,
                    p.id,
                    NotificationStatus::Suppressed,
                    next_attempt,
                    None,
                    None,
                )
                .await?;
            return Ok(());
        }
        let (status, error) = self.deliver(&channel_cfg, &notice).await;
        self.ops
            .mark_notification(
                p.org,
                p.id,
                status,
                next_attempt,
                error,
                (status == NotificationStatus::Sent).then(Utc::now),
            )
            .await?;
        Ok(())
    }

    /// Re-resolve the incident + monitor + channel for a retry, returning the
    /// incident's current state for the staleness check. `None` when any has
    /// since been deleted (the row then exhausts by attempts).
    async fn rebuild_notice(
        &self,
        org: OrgId,
        incident_id: Uuid,
        channel_id: Uuid,
        reason: NotificationReason,
    ) -> Result<Option<(IncidentNotice, crate::domain::ChannelConfig, IncidentState)>> {
        let Some(incident) = self.ops.get(org, incident_id).await? else {
            return Ok(None);
        };
        let Some(target_id) = incident.target_id else {
            return Ok(None);
        };
        let Some(target) = self.targets.get(org, target_id).await? else {
            return Ok(None);
        };
        let Some(channel) = self.channels.get(org, channel_id).await? else {
            return Ok(None);
        };
        Ok(Some((
            self.notice(&incident, &target, reason),
            channel.config.clone(),
            incident.state,
        )))
    }

    async fn deliver(
        &self,
        cfg: &crate::domain::ChannelConfig,
        notice: &IncidentNotice,
    ) -> (NotificationStatus, Option<String>) {
        match build_notifier(cfg, &self.http) {
            Ok(n) => match n.notify_incident(notice).await {
                Ok(()) => (NotificationStatus::Sent, None),
                Err(err) => (NotificationStatus::Failed, Some(err.to_string())),
            },
            Err(err) => (NotificationStatus::Failed, Some(err.to_string())),
        }
    }

    fn notice(
        &self,
        inc: &OpsIncident,
        target: &Target,
        reason: NotificationReason,
    ) -> IncidentNotice {
        IncidentNotice {
            incident_id: inc.id,
            reason,
            monitor_name: Some(target.name.clone()),
            title: inc.title.clone(),
            severity: inc.severity,
            urgency: inc.urgency,
            started_at: inc.started_at,
            ended_at: inc.ended_at,
            error_sample: inc.error_sample.clone(),
            url: self.deep_link(inc.id),
        }
    }

    fn deep_link(&self, id: Uuid) -> Option<String> {
        let base = self.base_url.trim_end_matches('/');
        (!base.is_empty()).then(|| format!("{base}/incidents/{id}"))
    }
}

/// The channel ids a policy rung pages. Only `channel` targets route today;
/// `user`/`schedule` targets are skipped until on-call resolution lands.
fn channels_at_level(policy: &EscalationPolicy, level: i32) -> Vec<Uuid> {
    let Some(step) = policy.steps.iter().find(|s| s.level == level) else {
        return vec![];
    };
    step.targets
        .iter()
        .filter_map(|t| match t.target_type {
            EscalationTargetType::Channel => t.channel_id,
            other => {
                tracing::debug!(
                    policy_id = %policy.id,
                    target_type = other.as_db_str(),
                    "escalation target type not yet routable; skipped"
                );
                None
            }
        })
        .collect()
}

/// The channel ids bound directly to a monitor (the pre-policy fallback path).
fn binding_channels(target: &Target) -> Vec<Uuid> {
    target.alerts.iter().map(|b| b.channel_id).collect()
}

/// Channels whose binding opted out of recovery notices.
fn recovery_opted_out(target: &Target) -> Vec<Uuid> {
    target
        .alerts
        .iter()
        .filter(|b| !b.notify_recovery)
        .map(|b| b.channel_id)
        .collect()
}

/// Is an outage already being paged? True when the most recent open-side page
/// (opened/reopened/escalated) is newer than the most recent resolution page —
/// i.e. we are inside an unresolved paging episode. Used to absorb duplicate
/// open signals without silencing a genuine reopen (which posts a Resolved row
/// first, ending the prior episode).
fn open_episode_active(rows: &[crate::domain::IncidentNotification]) -> bool {
    let last_open = rows
        .iter()
        .filter(|n| {
            matches!(
                n.reason,
                NotificationReason::Opened
                    | NotificationReason::Reopened
                    | NotificationReason::Escalated
            )
        })
        .map(|n| n.created_at)
        .max();
    let last_resolved = rows
        .iter()
        .filter(|n| n.reason == NotificationReason::Resolved)
        .map(|n| n.created_at)
        .max();
    match (last_open, last_resolved) {
        (Some(o), Some(r)) => o > r,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Channels to send the all-clear to: every channel paged this episode that
/// has not already been sent a resolution newer than its last open-side page.
fn resolvable_channels(rows: &[crate::domain::IncidentNotification]) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = Vec::new();
    let mut seen: Vec<Uuid> = Vec::new();
    for cid in rows.iter().filter_map(|n| n.channel_id) {
        if seen.contains(&cid) {
            continue;
        }
        seen.push(cid);
        let last_open = rows
            .iter()
            .filter(|n| {
                n.channel_id == Some(cid)
                    && matches!(
                        n.reason,
                        NotificationReason::Opened
                            | NotificationReason::Reopened
                            | NotificationReason::Escalated
                    )
            })
            .map(|n| n.created_at)
            .max();
        let Some(open_at) = last_open else { continue };
        let resolved_after = rows.iter().any(|n| {
            n.channel_id == Some(cid)
                && n.reason == NotificationReason::Resolved
                && n.created_at >= open_at
        });
        if !resolved_after {
            out.push(cid);
        }
    }
    out
}

/// A queued page becomes stale if the incident moved past the state it
/// describes before delivery succeeded: an outage notice once the incident is
/// resolved, or a recovery notice once it has reopened.
fn reason_is_stale(reason: NotificationReason, state: IncidentState) -> bool {
    match reason {
        NotificationReason::Opened
        | NotificationReason::Reopened
        | NotificationReason::Escalated => state == IncidentState::Resolved,
        NotificationReason::Resolved => state != IncidentState::Resolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration as StdDuration;

    use crate::domain::{
        AlertBinding, ChannelConfig, CheckSpec, EscalationTargetType, ExpectedStatus, HttpCheck,
        HttpMethod, IncidentOrigin, IncidentSeverity, IncidentState, IncidentUrgency,
        IncidentVisibility, NewEscalationPolicy, NewEscalationStep, NewEscalationTarget,
        NewNotificationChannel, OpsIncident, Target, TargetAlerts, WriteSource,
    };
    use crate::storage::{
        Actor, InMemoryEscalationPolicyStore, InMemoryIncidentOpsStore,
        InMemoryNotificationChannelStore, InMemoryTargetStore,
    };

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
                    config: ChannelConfig::Webhook {
                        url: "http://127.0.0.1:1/notify".into(),
                        headers: Default::default(),
                    },
                    enabled: true,
                },
                WriteSource::Ui,
                100,
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
            alerts: TargetAlerts(vec![AlertBinding {
                channel_id,
                after_failures: 1,
                notify_recovery,
            }]),
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
        let (_tx, rx) = mpsc::channel(4);
        EscalationEngine::new(
            rx,
            ops,
            policies,
            targets,
            channels,
            crate::http_outbound::build_outbound_client(
                crate::security::SsrfGuard::relaxed_for_tests(),
            ),
            EscalationConfig::default(),
            String::new(),
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
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
        // A duplicate Opened signal does not re-page the same episode.
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
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
        policies.set_target_policy(org(), tid, Some(p.id)).await.unwrap();

        let eng = engine(ops.clone(), policies, targets, channels);
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        let rows = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(rows.len(), 1);
        let inc = ops.get(org(), id).await.unwrap().unwrap();
        assert_eq!(inc.escalation_level, 1);
        assert_eq!(inc.escalation_policy_id, Some(p.id));
        assert!(inc.next_escalation_at.is_some(), "timer is armed for level 2");
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
        policies.set_target_policy(org(), tid, Some(p.id)).await.unwrap();

        let eng = engine(ops.clone(), policies, targets, channels);
        // Level 1 page + arm (delay 0 → immediately due).
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        assert_eq!(ops.get(org(), id).await.unwrap().unwrap().escalation_level, 1);

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
        policies.set_target_policy(org(), tid, Some(p.id)).await.unwrap();

        let eng = engine(ops.clone(), policies, targets, channels);
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        // A responder acks → next_escalation_at cleared, state acknowledged.
        ops.acknowledge(org(), id, Actor::System, None).await.unwrap();
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
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        ops.resolve(org(), id, Actor::System, None).await.unwrap();
        eng.page(org(), id, NotificationReason::Resolved).await.unwrap();
        assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 2);
        // A duplicate Resolved signal is absorbed.
        eng.page(org(), id, NotificationReason::Resolved).await.unwrap();
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
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        ops.resolve(org(), id, Actor::System, None).await.unwrap();
        eng.page(org(), id, NotificationReason::Resolved).await.unwrap();
        // Opened paged once; recovery opt-out blocked the resolution page.
        assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_incident_without_monitor_pages_nothing() {
        let channels = Arc::new(InMemoryNotificationChannelStore::new());
        let ops = Arc::new(InMemoryIncidentOpsStore::new());
        let id = seed_incident(&ops, None);
        let targets = Arc::new(InMemoryTargetStore::new());
        let policies = Arc::new(InMemoryEscalationPolicyStore::new());
        let eng = engine(ops.clone(), policies, targets, channels);
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
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
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        for _ in 0..10 {
            eng.retry_pending().await;
        }
        let rows = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].attempt, EscalationConfig::default().max_attempts as i32);
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
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        ops.resolve(org(), id, Actor::System, None).await.unwrap();
        eng.retry_pending().await;
        let rows = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, NotificationStatus::Suppressed);
    }
}
