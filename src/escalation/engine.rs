use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use moka::future::Cache;
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Concurrent in-flight signal pages — bounds a signal storm so one tenant's
/// burst cannot spawn unbounded delivery tasks.
const SIGNAL_CONCURRENCY: usize = 16;
/// Incidents escalated/retried concurrently within a sweep, so one slow channel
/// cannot serialise the whole tick.
const SWEEP_CONCURRENCY: usize = 8;
/// Fixed pool of per-incident locks (sharded by incident id) that serialise all
/// paging for one incident across the concurrent signal + sweep tasks, so a
/// reconcile and an inbound signal cannot both open the same episode and
/// double-page. Two distinct incidents may share a shard (harmless contention).
const PAGE_LOCK_SHARDS: usize = 256;
/// On-call roster cache lifetime. Short enough that an edit or a handoff is
/// reflected within seconds; long enough to collapse a sweep's repeated
/// resolutions of the same schedule into one query.
const ON_CALL_CACHE_TTL_SECS: u64 = 15;

use crate::config::EscalationConfig;
use crate::domain::{
    EscalationDecision, EscalationPolicy, EscalationTargetType, IncidentEventKind, IncidentState,
    NewIncidentNotification, NotificationReason, NotificationStatus, OpsIncident, OrgId, Target,
    UserId, next_step,
};
use crate::error::Result;
use crate::http_outbound::OutboundHttpClient;
use crate::notifier::build_notifier;
use crate::notifier::event::IncidentNotice;
use crate::storage::{
    Actor, ContactStore, DueIncident, EscalationPolicyStore, IncidentOpsStore,
    NotificationChannelStore, OnCallStore, PendingNotification, TargetStore,
};

/// One resolved paging destination: a concrete channel plus, when the rung
/// targeted a person or schedule, the responder it resolved to (recorded on the
/// notification row for the audit trail).
#[derive(Clone, Copy)]
struct PageTarget {
    channel_id: Uuid,
    user_id: Option<UserId>,
}

/// A nudge that an incident's state changed and its paging should be
/// reconciled. Carries no payload beyond identity + the reason to page; the
/// engine re-reads the incident, monitor, and channels so a stale signal never
/// pages outdated content.
pub struct IncidentSignal {
    pub org: OrgId,
    pub incident_id: Uuid,
    pub reason: NotificationReason,
}

/// Owns the rx loop. The paging work lives behind an `Arc<Worker>` so the loop
/// can dispatch signal handling and the periodic sweep onto detached tasks —
/// a slow channel can never stall `rx.recv()` (and with it every other
/// tenant's pages) the way an inline await would.
pub struct EscalationEngine {
    rx: mpsc::Receiver<IncidentSignal>,
    w: Arc<Worker>,
}

/// The shared paging core. Every field is cheap to clone/share; methods take
/// `&self` so they run from any task holding the `Arc`.
struct Worker {
    ops: Arc<dyn IncidentOpsStore>,
    policies: Arc<dyn EscalationPolicyStore>,
    on_call: Arc<dyn OnCallStore>,
    contacts: Arc<dyn ContactStore>,
    targets: Arc<dyn TargetStore>,
    channels: Arc<dyn NotificationChannelStore>,
    http: OutboundHttpClient,
    cfg: EscalationConfig,
    /// Operator base URL for incident deep links; empty omits the link.
    base_url: String,
    /// True while a sweep task is in flight, so overlapping ticks skip rather
    /// than stack a second sweep on top of a slow one.
    sweeping: AtomicBool,
    /// Sharded per-incident locks; `page()` holds one for the incident it
    /// touches so concurrent signal + sweep tasks serialise per incident.
    page_locks: Vec<Mutex<()>>,
    /// Short-TTL cache of resolved on-call rosters, keyed by schedule. Who is
    /// on call only changes at a handoff (>= daily), so a correlated outage
    /// paging many incidents off one schedule resolves it once per window
    /// instead of running the multi-query load per incident every tick.
    on_call_cache: Cache<(OrgId, Uuid), Arc<Vec<UserId>>>,
}

impl EscalationEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rx: mpsc::Receiver<IncidentSignal>,
        ops: Arc<dyn IncidentOpsStore>,
        policies: Arc<dyn EscalationPolicyStore>,
        on_call: Arc<dyn OnCallStore>,
        contacts: Arc<dyn ContactStore>,
        targets: Arc<dyn TargetStore>,
        channels: Arc<dyn NotificationChannelStore>,
        http: OutboundHttpClient,
        cfg: EscalationConfig,
        base_url: String,
    ) -> Self {
        Self {
            rx,
            w: Arc::new(Worker {
                ops,
                policies,
                on_call,
                contacts,
                targets,
                channels,
                http,
                cfg,
                base_url,
                sweeping: AtomicBool::new(false),
                page_locks: (0..PAGE_LOCK_SHARDS).map(|_| Mutex::new(())).collect(),
                on_call_cache: Cache::builder()
                    .max_capacity(10_000)
                    .time_to_live(Duration::from_secs(ON_CALL_CACHE_TTL_SECS))
                    .build(),
            }),
        }
    }

    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut tick =
            tokio::time::interval(Duration::from_secs(self.w.cfg.tick_interval_secs.max(1)));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Bounds concurrent signal-driven pages so a burst can't spawn without
        // limit; the sweep self-limits via SWEEP_CONCURRENCY + the budget.
        let sig_sem = Arc::new(Semaphore::new(SIGNAL_CONCURRENCY));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                maybe = self.rx.recv() => match maybe {
                    Some(sig) => {
                        let w = self.w.clone();
                        let sem = sig_sem.clone();
                        tokio::spawn(async move {
                            let _permit = sem.acquire().await;
                            if let Err(err) = w.page(sig.org, sig.incident_id, sig.reason).await {
                                tracing::warn!(incident_id = %sig.incident_id, error = %err, "incident paging failed");
                            }
                        });
                    }
                    None => return,
                },
                _ = tick.tick() => {
                    // Only one sweep at a time; a long sweep makes the next tick
                    // a no-op rather than piling on.
                    if self.w.sweeping.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                        let w = self.w.clone();
                        tokio::spawn(async move {
                            // Reset the flag on completion AND on unwind, so a
                            // panic in a sweep never wedges the engine into
                            // "always sweeping" (which would silently kill all
                            // future escalation/retry/reconcile).
                            struct ResetOnDrop(Arc<Worker>);
                            impl Drop for ResetOnDrop {
                                fn drop(&mut self) {
                                    self.0.sweeping.store(false, Ordering::Release);
                                }
                            }
                            let _reset = ResetOnDrop(w.clone());
                            w.sweep().await;
                        });
                    }
                }
            }
        }
    }

    // Test entry points — production drives these through `run` + the sweep.
    #[cfg(test)]
    async fn page(&self, org: OrgId, incident_id: Uuid, reason: NotificationReason) -> Result<()> {
        self.w.page(org, incident_id, reason).await
    }
    #[cfg(test)]
    async fn escalate_due(&self) {
        self.w.escalate_due().await
    }
    #[cfg(test)]
    async fn retry_pending(&self) {
        self.w.retry_pending().await
    }
    #[cfg(test)]
    async fn reconcile(&self) {
        self.w.reconcile().await
    }
}

impl Worker {
    /// Per-sweep wall-clock ceiling: a sweep never runs longer than one tick, so
    /// the loop returns to draining `rx` promptly even under slow channels.
    fn sweep_budget(&self) -> Duration {
        Duration::from_secs(self.cfg.tick_interval_secs.max(1))
    }

    /// The shard lock guarding paging for `incident_id`.
    fn page_lock(&self, incident_id: Uuid) -> &Mutex<()> {
        let shard = (incident_id.as_u128() % PAGE_LOCK_SHARDS as u128) as usize;
        &self.page_locks[shard]
    }

    /// Reconcile dropped open signals, then walk due escalations and retry
    /// failed pages. Runs on a detached task off the rx loop.
    async fn sweep(&self) {
        self.reconcile().await;
        self.escalate_due().await;
        self.retry_pending().await;
    }

    /// Catch incidents whose `Opened` signal was lost (e.g. the bounded signal
    /// channel saturated during a correlated mass outage): a `triggered`
    /// incident, older than the grace window, that was never paged and never
    /// armed. Re-running `page(Opened)` is idempotent — `open_episode` no-ops if
    /// the episode is already active. The DB is the source of truth, not the
    /// in-memory channel.
    async fn reconcile(&self) {
        let limit = self.cfg.max_pages_per_tick.max(1) as usize;
        let grace = chrono::Duration::seconds(self.cfg.tick_interval_secs.max(1) as i64);
        let cutoff = Utc::now() - grace;
        let due = match self.ops.due_for_reconcile(cutoff, limit).await {
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
    async fn page(&self, org: OrgId, incident_id: Uuid, reason: NotificationReason) -> Result<()> {
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
                &channel_targets(channels),
            )
            .await?;
        self.log_paged(org, incident.id, NotificationReason::Resolved, paged)
            .await
    }

    /// Walk the next rung of every due incident's policy, bounded-concurrent so
    /// one slow channel never serialises the tick, and budget-capped so the
    /// sweep returns promptly (the remainder is picked up next tick).
    async fn escalate_due(&self) {
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
        targets: &[PageTarget],
    ) -> Result<u32> {
        let mut paged = 0u32;
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

    /// Record on the timeline that a policy rung paged no one (empty schedule, a
    /// responder with no contacts, a deleted-and-cascaded channel) so the
    /// incident does not silently escalate past an unreachable rung.
    async fn note_empty_rung(&self, org: OrgId, incident_id: Uuid, level: i32) -> Result<()> {
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
        let Some((notice, channel_cfg, state)) = rebuilt else {
            self.ops
                .mark_notification(
                    p.org,
                    p.id,
                    NotificationStatus::Failed,
                    next_attempt,
                    None,
                    None,
                )
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
                Err(err) => (
                    NotificationStatus::Failed,
                    Some(redact_secrets(&err.to_string())),
                ),
            },
            Err(err) => (
                NotificationStatus::Failed,
                Some(redact_secrets(&err.to_string())),
            ),
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

    /// Resolve a schedule's current on-call roster, served from a short-TTL
    /// cache so a sweep paging many incidents off one schedule loads it once.
    async fn resolve_on_call(
        &self,
        org: OrgId,
        schedule_id: Uuid,
        at: chrono::DateTime<Utc>,
    ) -> Result<Arc<Vec<UserId>>> {
        if let Some(users) = self.on_call_cache.get(&(org, schedule_id)).await {
            return Ok(users);
        }
        let users = Arc::new(self.on_call.resolve_now(org, schedule_id, at).await?);
        // Don't cache an empty roster: a coverage gap an operator fixes
        // mid-incident must take effect next tick, not after the TTL.
        if !users.is_empty() {
            self.on_call_cache
                .insert((org, schedule_id), users.clone())
                .await;
        }
        Ok(users)
    }

    /// The concrete channels a policy rung pages, resolving each target type:
    /// a `channel` routes straight through; a `user` pages that responder's
    /// contact channels; a `schedule` resolves who is on call at `at` and pages
    /// each of their contact channels. Deduped by channel so one channel is
    /// paged at most once per rung (the first resolving responder is recorded).
    /// A target that resolves to nothing (no contacts, empty schedule) is
    /// skipped and logged.
    async fn resolve_targets(
        &self,
        org: OrgId,
        policy: &EscalationPolicy,
        level: i32,
        at: chrono::DateTime<Utc>,
    ) -> Result<Vec<PageTarget>> {
        let Some(step) = policy.steps.iter().find(|s| s.level == level) else {
            return Ok(vec![]);
        };
        let mut out: Vec<PageTarget> = Vec::new();
        for t in &step.targets {
            match t.target_type {
                EscalationTargetType::Channel => {
                    if let Some(cid) = t.channel_id {
                        push_target(&mut out, cid, None);
                    }
                }
                EscalationTargetType::User => {
                    if let Some(uid) = t.user_id {
                        self.page_user(org, UserId(uid), &mut out).await?;
                    }
                }
                EscalationTargetType::Schedule => {
                    if let Some(sid) = t.schedule_id {
                        for user in self.resolve_on_call(org, sid, at).await?.iter().copied() {
                            self.page_user(org, user, &mut out).await?;
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            // A rung that reaches no one is a live misconfiguration (empty
            // schedule, a responder with no contacts, a deleted channel) — the
            // incident escalates past it silently otherwise, so surface it.
            tracing::warn!(policy_id = %policy.id, level, "escalation rung resolved to no reachable channel");
        }
        Ok(out)
    }

    /// Append a responder's contact channels to `out`, attributing each to the
    /// user. A responder with no contact channels reaches no one — logged so a
    /// silently-unreachable on-call is visible in traces.
    async fn page_user(&self, org: OrgId, user: UserId, out: &mut Vec<PageTarget>) -> Result<()> {
        let contacts = self.contacts.for_user(org, user).await?;
        if contacts.is_empty() {
            tracing::warn!(%user, "on-call responder has no contact channels; not paged");
        }
        for cid in contacts {
            push_target(out, cid, Some(user));
        }
        Ok(())
    }
}

/// Strip the path/query/userinfo from any URL in a delivery error before it is
/// persisted to `incident_notifications.error`. A Slack webhook secret lives in
/// the path (`hooks.slack.com/services/T…/B…/<secret>`) and a Telegram bot
/// token in `…/bot<token>/…`; storing the raw transport error would leak them
/// at rest. Each whitespace token that parses as a URL is reduced to
/// `scheme://host[:port]`; everything else is kept verbatim.
fn redact_secrets(msg: &str) -> String {
    msg.split_whitespace()
        .map(|tok| {
            if !tok.contains("://") {
                return tok.to_string();
            }
            let trimmed = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != ':' && c != '/');
            match url::Url::parse(trimmed) {
                Ok(u) if u.host_str().is_some() => {
                    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
                    format!("{}://{}{}", u.scheme(), u.host_str().unwrap_or(""), port)
                }
                // Contains "://" but does not cleanly parse to a host — never
                // echo it verbatim (the secret-bearing path may survive); drop
                // the whole token.
                _ => "[redacted-url]".to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wrap bare channel ids (the no-policy fallback + resolution paths) as page
/// targets with no attributed responder.
fn channel_targets(channels: Vec<Uuid>) -> Vec<PageTarget> {
    channels
        .into_iter()
        .map(|channel_id| PageTarget {
            channel_id,
            user_id: None,
        })
        .collect()
}

/// Append a page target, deduped by channel so one channel is paged at most
/// once per rung. The first occurrence wins (it may carry a responder).
fn push_target(out: &mut Vec<PageTarget>, channel_id: Uuid, user_id: Option<UserId>) {
    if out.iter().any(|t| t.channel_id == channel_id) {
        return;
    }
    out.push(PageTarget {
        channel_id,
        user_id,
    });
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
        Actor, InMemoryContactStore, InMemoryEscalationPolicyStore, InMemoryIncidentOpsStore,
        InMemoryNotificationChannelStore, InMemoryOnCallStore, InMemoryTargetStore,
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
            ops,
            policies,
            on_call,
            contacts,
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
        eng.retry_pending().await;
        let rows = ops.notifications_for(org(), id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, NotificationStatus::Suppressed);
    }
}
