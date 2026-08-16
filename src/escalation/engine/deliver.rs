use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use crate::domain::{
    EscalationPolicy, EscalationTargetType, IncidentState, NotificationReason, NotificationStatus,
    OpsIncident, OrgId, Target, UserId,
};
use crate::error::Result;
use crate::notifier::event::IncidentNotice;
use crate::notifier::{EmailAlert, build_notifier};

use super::rules::{log_error_snippet, push_target, redact_secrets, retry_after_hint};
use super::{PageTarget, Worker};

impl Worker {
    /// Re-resolve the incident + monitor + channel for a retry, returning the
    /// incident's current state for the staleness check. `None` when any has
    /// since been deleted (the row then exhausts by attempts).
    pub(super) async fn rebuild_notice(
        &self,
        org: OrgId,
        incident_id: Uuid,
        channel_id: Uuid,
        reason: NotificationReason,
    ) -> Result<
        Option<(
            IncidentNotice,
            crate::domain::NotificationChannel,
            IncidentState,
        )>,
    > {
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
        let notice = self.notice(&incident, &target, reason, None);
        Ok(Some((notice, channel, incident.state)))
    }

    pub(super) async fn deliver(
        &self,
        org: OrgId,
        channel: &crate::domain::NotificationChannel,
        notice: &IncidentNotice,
        notification_id: Uuid,
        attempt: i32,
    ) -> (NotificationStatus, Option<String>, Option<String>) {
        let central = self.central_bot.as_ref().map(|c| c.as_central());
        let email_alert = self.email_alert(org, channel).await;
        let error = match build_notifier(
            &channel.config,
            &self.http,
            central,
            self.central_whatsapp.as_ref(),
            self.email.as_ref(),
            email_alert,
        ) {
            Ok(n) => match n.notify_incident(notice).await {
                Ok(()) => return (NotificationStatus::Sent, None, n.taken_receipt()),
                Err(err) => redact_secrets(&err.to_string()),
            },
            Err(err) => redact_secrets(&err.to_string()),
        };
        let snippet = log_error_snippet(&error);
        // A telegram throttle hint means the send was deferred, not broken —
        // info keeps the warn stream meaningful during a paging burst. Only
        // telegram transports get the downgrade: a webhook body echoing
        // "retry_after" is tenant-controlled and must not mute the warn.
        let deferred = matches!(
            channel.kind,
            crate::domain::ChannelKind::Telegram | crate::domain::ChannelKind::TelegramApp
        ) && retry_after_hint(Some(&error)).is_some();
        if deferred {
            tracing::info!(
                org_id = %org.0,
                incident_id = %notice.incident_id,
                channel_id = %channel.id,
                notification_id = %notification_id,
                transport = channel.kind.as_db_str(),
                attempt,
                error = %snippet,
                "incident notification deferred by transport"
            );
        } else {
            tracing::warn!(
                org_id = %org.0,
                incident_id = %notice.incident_id,
                channel_id = %channel.id,
                notification_id = %notification_id,
                transport = channel.kind.as_db_str(),
                attempt,
                error = %snippet,
                "incident notification delivery failed"
            );
        }
        (NotificationStatus::Failed, Some(error), None)
    }

    pub(super) fn notice(
        &self,
        inc: &OpsIncident,
        target: &Target,
        reason: NotificationReason,
        note: Option<String>,
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
            // Open-time snapshot off the incident — no per-page region query.
            regions_down: inc.regions_down.clone(),
            regions_up: inc.regions_up.clone(),
            url: self.deep_link(inc.id),
            note,
        }
    }

    fn deep_link(&self, id: Uuid) -> Option<String> {
        let base = self.base_url.trim_end_matches('/');
        (!base.is_empty()).then(|| format!("{base}/incidents/{id}"))
    }

    async fn email_alert(
        &self,
        org: OrgId,
        channel: &crate::domain::NotificationChannel,
    ) -> Option<EmailAlert> {
        crate::notifier::email_alert_for(
            self.orgs.as_ref(),
            &self.base_url,
            &self.alert_channel_stop_secret,
            org,
            channel,
        )
        .await
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
    pub(super) async fn resolve_targets(
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
