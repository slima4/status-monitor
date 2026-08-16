use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::time::Instant;
use uuid::Uuid;

use crate::domain::{
    EscalationDecision, IncidentEventKind, IncidentState, NewIncidentNotification,
    NotificationOutcome, NotificationReason, NotificationStatus, OrgId, next_step,
};
use crate::error::Result;
use crate::notifier::event::IncidentNotice;
use crate::storage::{Actor, DueIncident, PendingNotification};

use super::rules::{Paged, channel_targets, reason_is_stale, resolvable_channels};
use super::{PageTarget, SWEEP_CONCURRENCY, Worker};

impl Worker {
    /// Walk the next rung of every due incident's policy, bounded-concurrent so
    /// one slow channel never serialises the tick, and budget-capped so the
    /// sweep returns promptly (the remainder is picked up next tick).
    pub(super) async fn escalate_due(&self) {
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        // Lease claimed rungs long enough to page + record the real next time
        // before another instance could re-pick them.
        let lease = (self.cfg.tick_interval_secs.max(1) as i64 * 2).max(60);
        let due = match self.ops.due_for_escalation(Utc::now(), limit, lease).await {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "escalation sweep scan failed");
                return;
            }
        };
        let budget = self.sweep_budget();
        let start = Instant::now();
        let mut it = due.into_iter();
        let mut futs = FuturesUnordered::new();
        for d in it.by_ref().take(SWEEP_CONCURRENCY) {
            futs.push(self.escalate_one_logged(d));
        }
        while futs.next().await.is_some() {
            if start.elapsed() < budget
                && let Some(d) = it.next()
            {
                futs.push(self.escalate_one_logged(d));
            }
        }
    }

    async fn escalate_one_logged(&self, d: DueIncident) {
        if let Err(err) = self.escalate_one(&d).await {
            tracing::warn!(incident_id = %d.id, error = %err, "escalation step failed");
        }
    }

    /// Re-page the channels of open, unacknowledged incidents whose monitor's
    /// reminder interval has elapsed since the last page. Bounded-concurrent +
    /// budget-capped like the escalation sweep. Unlike `escalate_due` this takes
    /// no claim-and-lease: a reminder is idempotent in intent, so a duplicate
    /// across two engine instances is a harmless extra page (not a skipped or
    /// double-advanced rung), and the single-tick single-flight guard already
    /// prevents an instance from reminding the same incident twice per interval.
    pub(super) async fn renotify_due(&self) {
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let due = match self.ops.due_for_renotify(Utc::now(), limit).await {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "renotify scan failed");
                return;
            }
        };
        let budget = self.sweep_budget();
        let start = Instant::now();
        let mut it = due.into_iter();
        let mut futs = FuturesUnordered::new();
        for d in it.by_ref().take(SWEEP_CONCURRENCY) {
            futs.push(self.renotify_one_logged(d));
        }
        while futs.next().await.is_some() {
            if start.elapsed() < budget
                && let Some(d) = it.next()
            {
                futs.push(self.renotify_one_logged(d));
            }
        }
    }

    async fn renotify_one_logged(&self, d: DueIncident) {
        if let Err(err) = self.renotify_one(&d).await {
            tracing::warn!(incident_id = %d.id, error = %err, "incident reminder failed");
        }
    }

    /// Re-page the channels already notified this episode for a still-open,
    /// unacknowledged incident. Re-reads state under the per-incident lock so an
    /// ack/resolve landing after the scan silences the reminder; the fresh page
    /// advances the last-paged time, so the next reminder is one interval out.
    pub(super) async fn renotify_one(&self, d: &DueIncident) -> Result<()> {
        let _guard = self.page_lock(d.id).lock().await;
        let Some(incident) = self.ops.get(d.org, d.id).await? else {
            return Ok(());
        };
        if incident.state != IncidentState::Triggered {
            return Ok(());
        }
        let Some(target_id) = incident.target_id else {
            return Ok(());
        };
        let Some(target) = self.targets.get(d.org, target_id).await? else {
            return Ok(());
        };
        if target.renotify_interval_secs == 0 {
            return Ok(());
        }
        let channels = resolvable_channels(&self.ops.notifications_for(d.org, d.id).await?);
        if channels.is_empty() {
            return Ok(());
        }
        let notice = self.notice(&incident, &target, NotificationReason::Opened, None);
        let paged = self
            .page_channels(
                d.org,
                d.id,
                &notice,
                NotificationReason::Opened,
                incident.escalation_level,
                &channel_targets(channels),
            )
            .await?;
        self.log_paged(d.org, d.id, NotificationReason::Opened, paged.delivered)
            .await
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
                let notice = self.notice(&incident, &target, NotificationReason::Escalated, None);
                let targets = self
                    .resolve_targets(d.org, &policy, level, Utc::now())
                    .await?;
                if targets.is_empty() {
                    self.note_empty_rung(d.org, d.id, level).await?;
                }
                let paged = self
                    .page_channels(
                        d.org,
                        d.id,
                        &notice,
                        NotificationReason::Escalated,
                        level,
                        &targets,
                    )
                    .await?;
                let next_at = Some(Utc::now() + chrono::Duration::seconds(delay_secs.into()));
                self.ops
                    .record_escalation(d.org, d.id, level, round, next_at)
                    .await?;
                self.log_paged(d.org, d.id, NotificationReason::Escalated, paged.delivered)
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
    pub(super) async fn page_channels(
        &self,
        org: OrgId,
        incident_id: Uuid,
        notice: &IncidentNotice,
        reason: NotificationReason,
        level: i32,
        targets: &[PageTarget],
    ) -> Result<Paged> {
        let mut paged = Paged::default();
        for t in targets {
            let cid = t.channel_id;
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
                    target_user_id: t.user_id,
                    channel_id: Some(cid),
                    transport: channel.kind.as_db_str().to_string(),
                    reason,
                    status: NotificationStatus::Queued,
                    attempt: 1,
                    error: None,
                    sent_at: None,
                })
                .await?;
            // Unverified email: no send is attempted, but the row records a
            // failure so the gap is visible on the incident, not silent.
            let (status, error, receipt) = if channel.awaiting_verification() {
                (
                    NotificationStatus::Failed,
                    Some("email address not verified".to_string()),
                    None,
                )
            } else {
                self.deliver(org, &channel, notice, id, 1).await
            };
            paged.recorded += 1;
            if status == NotificationStatus::Sent {
                paged.delivered += 1;
            }
            // A first-attempt failure schedules the backoff so the retry sweep
            // waits instead of re-firing next tick.
            let next_attempt_at = (status == NotificationStatus::Failed)
                .then(|| self.retry_backoff_hinted(1, error.as_deref()))
                .flatten();
            if status == NotificationStatus::Failed {
                self.note_dead_letter(channel.kind.as_db_str(), next_attempt_at);
            }
            self.ops
                .mark_notification(
                    org,
                    id,
                    NotificationOutcome {
                        status,
                        attempt: 1,
                        error,
                        sent_at: (status == NotificationStatus::Sent).then(Utc::now),
                        next_attempt_at,
                        provider_receipt: receipt,
                    },
                )
                .await?;
        }
        Ok(paged)
    }

    pub(super) async fn log_paged(
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

    /// Record on the timeline that a policy rung paged no one (empty schedule, a
    /// responder with no contacts, a deleted-and-cascaded channel) so the
    /// incident does not silently escalate past an unreachable rung.
    pub(super) async fn note_empty_rung(
        &self,
        org: OrgId,
        incident_id: Uuid,
        level: i32,
    ) -> Result<()> {
        self.ops
            .append_event(
                org,
                incident_id,
                IncidentEventKind::Escalated,
                Actor::System,
                Some(format!(
                    "escalation level {level} reached no channel — check the policy's targets"
                )),
            )
            .await
    }

    /// Re-attempt failed deliveries under the attempt cap. Each retry updates
    /// the existing row rather than inserting a new one. Bounded-concurrent +
    /// budget-capped like the escalation sweep.
    pub(super) async fn retry_pending(&self) {
        let max_attempts = self.cfg.max_attempts as i32;
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let pending = match self
            .ops
            .pending_notifications(Utc::now(), limit, max_attempts)
            .await
        {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(error = %err, "escalation retry scan failed");
                return;
            }
        };
        let budget = self.sweep_budget();
        let start = Instant::now();
        let mut it = pending.into_iter();
        let mut futs = FuturesUnordered::new();
        for p in it.by_ref().take(SWEEP_CONCURRENCY) {
            futs.push(self.retry_one_logged(p));
        }
        while futs.next().await.is_some() {
            if start.elapsed() < budget
                && let Some(p) = it.next()
            {
                futs.push(self.retry_one_logged(p));
            }
        }
    }

    async fn retry_one_logged(&self, p: PendingNotification) {
        if let Err(err) = self.retry_one(&p).await {
            tracing::warn!(notification_id = %p.id, error = %err, "incident page retry failed");
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
            Some(cid) => {
                self.rebuild_notice(p.org, p.incident_id, cid, p.reason)
                    .await?
            }
            None => None,
        };
        let Some((notice, channel, state)) = rebuilt else {
            // Channel/incident gone: back off so a dead target doesn't churn
            // every tick, and let the attempt cap retire the row.
            let next_attempt_at = self.retry_backoff(next_attempt);
            self.note_dead_letter(&p.transport, next_attempt_at);
            self.ops
                .mark_notification(
                    p.org,
                    p.id,
                    NotificationOutcome {
                        status: NotificationStatus::Failed,
                        attempt: next_attempt,
                        error: None,
                        sent_at: None,
                        next_attempt_at,
                        provider_receipt: None,
                    },
                )
                .await?;
            return Ok(());
        };
        if reason_is_stale(p.reason, state) {
            // Terminal: the page no longer matches the incident state.
            self.ops
                .mark_notification(
                    p.org,
                    p.id,
                    NotificationOutcome {
                        status: NotificationStatus::Suppressed,
                        attempt: next_attempt,
                        error: None,
                        sent_at: None,
                        next_attempt_at: None,
                        provider_receipt: None,
                    },
                )
                .await?;
            return Ok(());
        }
        let (status, error, receipt) = if channel.awaiting_verification() {
            (
                NotificationStatus::Failed,
                Some("email address not verified".to_string()),
                None,
            )
        } else {
            self.deliver(p.org, &channel, &notice, p.id, next_attempt)
                .await
        };
        let next_attempt_at = (status == NotificationStatus::Failed)
            .then(|| self.retry_backoff_hinted(next_attempt, error.as_deref()))
            .flatten();
        if status == NotificationStatus::Failed {
            self.note_dead_letter(&p.transport, next_attempt_at);
        }
        self.ops
            .mark_notification(
                p.org,
                p.id,
                NotificationOutcome {
                    status,
                    attempt: next_attempt,
                    error,
                    sent_at: (status == NotificationStatus::Sent).then(Utc::now),
                    next_attempt_at,
                    provider_receipt: receipt,
                },
            )
            .await?;
        Ok(())
    }
}
