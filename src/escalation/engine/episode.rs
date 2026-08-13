use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::time::Instant;
use uuid::Uuid;

use crate::domain::{
    ChannelConfig, EscalationDecision, IncidentEventKind, NotificationReason, OpsIncident, OrgId,
    Target, next_step,
};
use crate::error::Result;
use crate::notifier::pushover::PushoverReceipts;
use crate::storage::{Actor, DueIncident, EmergencyAck};

use super::rules::{binding_channels, channel_targets, open_episode_active, resolvable_channels};
use super::{SWEEP_CONCURRENCY, Worker};

impl Worker {
    /// Reconcile dropped open signals, then walk due escalations and retry
    /// failed pages. Runs on a detached task off the rx loop.
    pub(super) async fn sweep(&self) {
        self.reconcile().await;
        self.escalate_due().await;
        self.renotify_due().await;
        self.retry_pending().await;
        self.poll_acks().await;
    }

    /// Catch incidents whose `Opened` signal was lost (e.g. the bounded signal
    /// channel saturated during a correlated mass outage): a `triggered`
    /// incident, older than the grace window, that was never paged and never
    /// armed. Re-running `page(Opened)` is idempotent — `open_episode` no-ops if
    /// the episode is already active. The DB is the source of truth, not the
    /// in-memory channel. Bounded on both sides: a monitor with no channel
    /// bound records nothing, so it would otherwise match this scan forever.
    pub(super) async fn reconcile(&self) {
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let now = Utc::now();
        let grace = chrono::Duration::seconds(self.cfg.tick_interval_secs.max(1) as i64);
        let window = chrono::Duration::seconds(self.cfg.reconcile_window_secs.max(1) as i64);
        let due = match self
            .ops
            .due_for_reconcile((now - window, now - grace), limit)
            .await
        {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "escalation reconcile scan failed");
                return;
            }
        };
        if !due.is_empty() {
            tracing::warn!(
                count = due.len(),
                "reconciling incidents that were never paged"
            );
        }
        let budget = self.sweep_budget();
        let start = Instant::now();
        let mut it = due.into_iter();
        let mut futs = FuturesUnordered::new();
        for d in it.by_ref().take(SWEEP_CONCURRENCY) {
            futs.push(self.reconcile_one_logged(d));
        }
        while futs.next().await.is_some() {
            if start.elapsed() < budget
                && let Some(d) = it.next()
            {
                futs.push(self.reconcile_one_logged(d));
            }
        }
    }

    async fn reconcile_one_logged(&self, d: DueIncident) {
        if let Err(err) = self.page(d.org, d.id, NotificationReason::Opened).await {
            tracing::warn!(incident_id = %d.id, error = %err, "incident reconcile page failed");
        }
    }

    /// Handle a lifecycle signal. Opened/Reopened start the escalation episode
    /// (page the first rung, arm the timer); Resolved notifies the channels
    /// already paged this episode. The escalation sweep handles later rungs.
    pub(super) async fn page(
        &self,
        org: OrgId,
        incident_id: Uuid,
        reason: NotificationReason,
    ) -> Result<()> {
        // Serialise all paging for this incident: the dedup in open_episode /
        // notify_resolution is read-then-act, so without this a concurrent
        // signal task + sweep (reconcile) task could both open the same episode
        // and double-page. Held for the whole resolve+page+record sequence.
        let _guard = self.page_lock(incident_id).lock().await;
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
            // Silence has no incident row; the silence sweep delivers it.
            NotificationReason::NoData | NotificationReason::DataResumed => Ok(()),
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
                        let targets = self
                            .resolve_targets(org, &policy, level, Utc::now())
                            .await?;
                        if targets.is_empty() {
                            self.note_empty_rung(org, incident.id, level).await?;
                        }
                        let paged = self
                            .page_channels(org, incident.id, &notice, reason, level, &targets)
                            .await?;
                        let next_at =
                            Some(Utc::now() + chrono::Duration::seconds(delay_secs.into()));
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
                let targets = channel_targets(binding_channels(target));
                let paged = self
                    .page_channels(org, incident.id, &notice, reason, 0, &targets)
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
        // Stop any still-repeating emergency page before the recovery-notice
        // gate — a resolved incident must go quiet even when the recovery push
        // itself is disabled.
        self.cancel_emergency(org, incident.id).await;
        if !target.notify_recovery {
            return Ok(());
        }
        let rows = self.ops.notifications_for(org, incident.id).await?;
        let channels: Vec<Uuid> = resolvable_channels(&rows);
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
                &channel_targets(channels),
            )
            .await?;
        self.log_paged(org, incident.id, NotificationReason::Resolved, paged)
            .await
    }

    /// Build a Pushover receipt client from the channel's stored application
    /// token. `None` if the channel is gone or not a Pushover channel.
    async fn pushover_receipts(&self, org: OrgId, channel_id: Uuid) -> Option<PushoverReceipts> {
        let channel = self.channels.get(org, channel_id).await.ok()??;
        match &channel.config {
            ChannelConfig::Pushover(c) => {
                Some(PushoverReceipts::new(self.http.clone(), c.token.clone()))
            }
            _ => None,
        }
    }

    /// Cancel every outstanding emergency page for a resolved incident so it
    /// stops repeating. Best-effort: a failed cancel still clears the receipt so
    /// the poll sweep doesn't keep chasing it; Pushover's `expire` is the
    /// backstop.
    async fn cancel_emergency(&self, org: OrgId, incident_id: Uuid) {
        let acks = match self.ops.emergency_acks_for_incident(org, incident_id).await {
            Ok(a) => a,
            Err(err) => {
                tracing::warn!(error = %err, "emergency ack lookup failed");
                return;
            }
        };
        for ack in acks {
            if let Some(client) = self.pushover_receipts(org, ack.channel_id).await
                && let Err(err) = client.cancel(&ack.receipt).await
            {
                tracing::warn!(incident_id = %incident_id, error = %err, "pushover emergency cancel failed");
            }
            if let Err(err) = self.ops.clear_receipt(org, ack.id).await {
                tracing::warn!(error = %err, "clearing emergency receipt failed");
            }
        }
    }

    /// Poll outstanding emergency receipts: record acknowledgement on the
    /// timeline, drop the receipt once acked or expired so it leaves the poll
    /// set.
    async fn poll_acks(&self) {
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let due = match self.ops.due_emergency_acks(limit).await {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "emergency ack poll scan failed");
                return;
            }
        };
        let budget = self.sweep_budget();
        let start = Instant::now();
        let mut it = due.into_iter();
        let mut futs = FuturesUnordered::new();
        for a in it.by_ref().take(SWEEP_CONCURRENCY) {
            futs.push(self.poll_one_ack(a));
        }
        while futs.next().await.is_some() {
            if start.elapsed() < budget
                && let Some(a) = it.next()
            {
                futs.push(self.poll_one_ack(a));
            }
        }
    }

    async fn poll_one_ack(&self, ack: EmergencyAck) {
        let Some(client) = self.pushover_receipts(ack.org, ack.channel_id).await else {
            // Channel gone: nothing can cancel or read it — retire the receipt.
            let _ = self.ops.clear_receipt(ack.org, ack.id).await;
            return;
        };
        let state = match client.poll(&ack.receipt).await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, "pushover receipt poll failed");
                return;
            }
        };
        if state.acknowledged {
            if let Err(err) = self
                .ops
                .append_event(
                    ack.org,
                    ack.incident_id,
                    IncidentEventKind::Note,
                    Actor::System,
                    Some("emergency page acknowledged in Pushover".to_string()),
                )
                .await
            {
                tracing::warn!(error = %err, "recording emergency ack failed");
            }
            let _ = self.ops.mark_acked(ack.org, ack.id, Utc::now()).await;
        } else if state.expired {
            let _ = self.ops.clear_receipt(ack.org, ack.id).await;
        }
    }
}
