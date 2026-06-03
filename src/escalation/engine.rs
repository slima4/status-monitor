use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::EscalationConfig;
use crate::domain::{
    IncidentEventKind, IncidentState, NewIncidentNotification, NotificationReason,
    NotificationStatus, OpsIncident, OrgId, Target,
};
use crate::error::Result;
use crate::http_outbound::OutboundHttpClient;
use crate::notifier::build_notifier;
use crate::notifier::event::IncidentNotice;
use crate::storage::{
    Actor, IncidentOpsStore, NotificationChannelStore, PendingNotification, TargetStore,
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
        targets: Arc<dyn TargetStore>,
        channels: Arc<dyn NotificationChannelStore>,
        http: OutboundHttpClient,
        cfg: EscalationConfig,
        base_url: String,
    ) -> Self {
        Self {
            rx,
            ops,
            targets,
            channels,
            http,
            cfg,
            base_url,
        }
    }

    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut sweep =
            tokio::time::interval(Duration::from_secs(self.cfg.tick_interval_secs.max(1)));
        sweep.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
                _ = sweep.tick() => self.retry_pending().await,
            }
        }
    }

    /// Page each bound channel for this `(incident, reason)`. A channel is
    /// skipped when its most recent delivery row already carries this reason —
    /// so a duplicate signal is a no-op, but a later reason (after a reopen)
    /// still pages. The row is persisted `queued` before the send so a crash or
    /// record failure can never leave a delivered page with no dedup/audit row.
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
        if target.alerts.is_empty() {
            return Ok(());
        }
        let already = self.ops.notifications_for(org, incident_id).await?;
        let notice = self.notice(&incident, &target, reason);
        let mut paged = 0u32;
        for binding in target.alerts.iter() {
            let cid = binding.channel_id;
            // A recovery notice respects the binding's opt-out.
            if reason == NotificationReason::Resolved && !binding.notify_recovery {
                continue;
            }
            // Dedup on the channel's latest reason (rows are ordered ascending),
            // so repeat signals are absorbed but a new transition still pages.
            if already
                .iter()
                .rev()
                .find(|n| n.channel_id == Some(cid))
                .map(|n| n.reason)
                == Some(reason)
            {
                continue;
            }
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
                    escalation_level: Some(incident.escalation_level),
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
            let (status, error) = self.deliver(&channel.config, &notice).await;
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
        if paged > 0 {
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
                .await?;
        }
        Ok(())
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
        AlertBinding, ChannelConfig, CheckSpec, ExpectedStatus, HttpCheck, HttpMethod,
        IncidentOrigin, IncidentSeverity, IncidentState, IncidentUrgency, IncidentVisibility,
        NewNotificationChannel, OpsIncident, Target, TargetAlerts, WriteSource,
    };
    use crate::storage::{
        Actor, InMemoryIncidentOpsStore, InMemoryNotificationChannelStore, InMemoryTargetStore,
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
                    name: "ops".into(),
                    config: ChannelConfig::Webhook {
                        url: "http://127.0.0.1:1/notify".into(),
                        headers: Default::default(),
                    },
                    enabled: true,
                },
                WriteSource::Ui,
                10,
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

    fn engine(
        ops: Arc<dyn IncidentOpsStore>,
        targets: Arc<dyn TargetStore>,
        channels: Arc<dyn NotificationChannelStore>,
    ) -> EscalationEngine {
        let (_tx, rx) = mpsc::channel(4);
        EscalationEngine::new(
            rx,
            ops,
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
    async fn page_records_one_attempt_per_channel_and_dedups_repeat_signals() {
        let channels = Arc::new(InMemoryNotificationChannelStore::new());
        let cid = failing_channel(&channels).await;
        let target = target_with_channel(cid);
        let tid = target.id;
        let ops = Arc::new(InMemoryIncidentOpsStore::new());
        let id = seed_incident(&ops, Some(tid));
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));

        let eng = engine(ops.clone(), targets, channels);
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        let after_first = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(after_first.len(), 1, "one delivery row for the bound channel");
        assert_eq!(after_first[0].status, NotificationStatus::Failed);
        assert_eq!(after_first[0].reason, NotificationReason::Opened);

        // A duplicate signal for the same (incident, reason) must not re-page.
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        assert_eq!(
            ops.notifications_for(org(), id).await.unwrap().len(),
            1,
            "repeat Opened signal is deduped"
        );

        // A different reason is a distinct page.
        eng.page(org(), id, NotificationReason::Resolved).await.unwrap();
        assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn alternating_reasons_repage_so_reopen_cycles_are_not_silenced() {
        let channels = Arc::new(InMemoryNotificationChannelStore::new());
        let cid = failing_channel(&channels).await;
        let target = target_with_channel(cid);
        let tid = target.id;
        let ops = Arc::new(InMemoryIncidentOpsStore::new());
        let id = seed_incident(&ops, Some(tid));
        let targets = Arc::new(InMemoryTargetStore::from_vec(vec![target]));
        let eng = engine(ops.clone(), targets, channels);

        async fn page_count(
            eng: &EscalationEngine,
            ops: &InMemoryIncidentOpsStore,
            id: Uuid,
            reason: NotificationReason,
        ) -> usize {
            eng.page(org(), id, reason).await.unwrap();
            ops.notifications_for(org(), id).await.unwrap().len()
        }
        use NotificationReason::{Opened, Resolved};
        // open, resolve, repeat-resolve (deduped), reopen, resolve again.
        assert_eq!(page_count(&eng, &ops, id, Opened).await, 1);
        assert_eq!(page_count(&eng, &ops, id, Resolved).await, 2);
        assert_eq!(page_count(&eng, &ops, id, Resolved).await, 2, "repeat is deduped");
        assert_eq!(page_count(&eng, &ops, id, Opened).await, 3, "reopen re-pages");
        assert_eq!(
            page_count(&eng, &ops, id, Resolved).await,
            4,
            "second resolution after a reopen must page, not dedup against the first"
        );
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
        let eng = engine(ops.clone(), targets, channels);

        eng.page(org(), id, NotificationReason::Resolved).await.unwrap();
        assert!(
            ops.notifications_for(org(), id).await.unwrap().is_empty(),
            "a binding opted out of recovery gets no resolved page"
        );
        // An outage page still goes through.
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        assert_eq!(ops.notifications_for(org(), id).await.unwrap().len(), 1);
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
        let eng = engine(ops.clone(), targets, channels);

        // Opened page fails and is queued for retry.
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();
        // The incident resolves before the retry lands.
        ops.resolve(org(), id, Actor::System, None).await.unwrap();
        eng.retry_pending().await;

        let rows = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].status,
            NotificationStatus::Suppressed,
            "a stale 'OPEN' page must not be re-sent after the incident resolved"
        );
    }

    #[tokio::test]
    async fn manual_incident_without_monitor_pages_nothing() {
        let channels = Arc::new(InMemoryNotificationChannelStore::new());
        let ops = Arc::new(InMemoryIncidentOpsStore::new());
        let id = seed_incident(&ops, None);
        let targets = Arc::new(InMemoryTargetStore::new());
        let eng = engine(ops.clone(), targets, channels);
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

        let eng = engine(ops.clone(), targets, channels);
        eng.page(org(), id, NotificationReason::Opened).await.unwrap();

        // Default max_attempts = 5; first attempt is 1. Each sweep bumps by one.
        for _ in 0..10 {
            eng.retry_pending().await;
        }
        let rows = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].attempt,
            EscalationConfig::default().max_attempts as i32,
            "retries stop once the attempt cap is reached"
        );
        assert_eq!(rows[0].status, NotificationStatus::Failed);
    }
}
