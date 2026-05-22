use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use metrics::counter;
use moka::sync::Cache;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::{CheckStatus, NotificationChannel, OrgId};
use crate::notifier::build_notifier;
use crate::notifier::event::{AlertEvent, AlertKind, AlertSignal};
use crate::observability::metrics::names;
use crate::storage::NotificationChannelStore;

/// How long a resolved (or absent) channel is cached. The alert path runs on
/// every check result; without this each result would re-query every bound
/// channel. Tradeoff to accept: an edit that *disables or deletes* a channel
/// (e.g. revoking a leaked Slack webhook) still delivers to the old
/// destination for up to this window, and a newly-created/re-enabled channel
/// can take up to this long to start firing. 30s keeps both bounded.
const CHANNEL_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Clone, Copy)]
struct AlertState {
    consecutive_non_up: u32,
    alerting: bool,
}

/// Pure threshold/recovery decision. Extracted so the state machine is unit
/// tested without a notifier or a store. Mutates `entry` and returns the
/// event to emit (if any) plus the failure count to report on it.
fn decide(
    entry: &mut AlertState,
    is_up: bool,
    after: u32,
    notify_recovery: bool,
) -> (Option<AlertKind>, u32) {
    match (is_up, entry.alerting) {
        (true, true) => {
            let prev_failures = entry.consecutive_non_up;
            *entry = AlertState::default();
            (
                notify_recovery.then_some(AlertKind::Recovered),
                prev_failures,
            )
        }
        (true, false) => {
            entry.consecutive_non_up = 0;
            (None, 0)
        }
        (false, false) => {
            entry.consecutive_non_up = entry.consecutive_non_up.saturating_add(1);
            if entry.consecutive_non_up >= after.max(1) {
                entry.alerting = true;
                (Some(AlertKind::Down), entry.consecutive_non_up)
            } else {
                (None, 0)
            }
        }
        (false, true) => {
            entry.consecutive_non_up = entry.consecutive_non_up.saturating_add(1);
            (None, 0)
        }
    }
}

pub struct AlertEngine {
    rx: mpsc::Receiver<AlertSignal>,
    channels: Arc<dyn NotificationChannelStore>,
    http: crate::http_outbound::OutboundHttpClient,
    state: Arc<Mutex<HashMap<(Uuid, Uuid), AlertState>>>,
    /// Keyed by `(org, channel_id)` so a tenant's binding can only ever hit
    /// that tenant's channel. `None` is cached too, so a binding to a deleted
    /// channel doesn't hammer the store on every check.
    cache: Cache<(OrgId, Uuid), Option<Arc<NotificationChannel>>>,
}

impl AlertEngine {
    pub fn new(
        rx: mpsc::Receiver<AlertSignal>,
        channels: Arc<dyn NotificationChannelStore>,
        http: crate::http_outbound::OutboundHttpClient,
    ) -> Self {
        Self {
            rx,
            channels,
            http,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache: Cache::builder()
                .time_to_live(CHANNEL_CACHE_TTL)
                .max_capacity(4096)
                .build(),
        }
    }

    pub async fn run(mut self, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                maybe = self.rx.recv() => {
                    match maybe {
                        Some(signal) => self.handle(signal).await,
                        None => return,
                    }
                }
            }
        }
    }

    /// Resolve a bound channel id to its (org-scoped) channel. Cached for
    /// `CHANNEL_CACHE_TTL`; a store error is logged and treated as "absent"
    /// for this tick but not cached, so the next result retries.
    async fn resolve(&self, org: OrgId, channel_id: Uuid) -> Option<Arc<NotificationChannel>> {
        // Defense in depth: a nil org is the in-memory test fixture's
        // unset-org sentinel, never a real tenant. Refuse to resolve it so a
        // mis-wired alert fan-out can never cross-bind the nil org to a real
        // channel, regardless of the prose invariant upstream.
        debug_assert!(!org.0.is_nil(), "alert resolve called with nil org");
        if org.0.is_nil() {
            return None;
        }
        if let Some(hit) = self.cache.get(&(org, channel_id)) {
            return hit;
        }
        match self.channels.get(org, channel_id).await {
            Ok(opt) => {
                let val = opt.map(Arc::new);
                self.cache.insert((org, channel_id), val.clone());
                val
            }
            Err(err) => {
                tracing::warn!(%channel_id, error = %err, "resolving notification channel failed");
                None
            }
        }
    }

    async fn handle(&self, signal: AlertSignal) {
        let target = signal.target;
        if target.alerts.is_empty() {
            return;
        }
        let org = signal.org_id;
        let is_up = signal.result.status == CheckStatus::Up;
        for binding in target.alerts.iter() {
            let Some(channel) = self.resolve(org, binding.channel_id).await else {
                continue;
            };
            if !channel.enabled {
                continue;
            }
            let (event_kind, failures_at_event) = {
                let mut guard = self.state.lock();
                let entry = guard.entry((target.id, binding.channel_id)).or_default();
                decide(
                    entry,
                    is_up,
                    binding.after_failures,
                    binding.notify_recovery,
                )
            };
            let Some(kind) = event_kind else { continue };

            let event = AlertEvent {
                target_id: target.id,
                target_name: target.name.clone(),
                kind,
                consecutive_failures: failures_at_event,
                last_status: signal.result.status,
                last_error: signal.result.error.clone(),
                timestamp: Utc::now(),
            };
            let channel_label = channel.kind.as_db_str();
            counter!(
                names::NOTIFICATIONS_TOTAL,
                "channel" => channel_label,
                "kind" => kind.as_str(),
            )
            .increment(1);
            let notifier = match build_notifier(&channel.config, &self.http) {
                Ok(n) => n,
                Err(err) => {
                    counter!(names::NOTIFICATIONS_FAILURES, "channel" => channel_label)
                        .increment(1);
                    tracing::warn!(target_id = %target.id, channel_id = %binding.channel_id, error = %err, "building notifier failed");
                    continue;
                }
            };
            if let Err(err) = notifier.notify(&event).await {
                counter!(names::NOTIFICATIONS_FAILURES, "channel" => channel_label).increment(1);
                tracing::warn!(
                    target_id = %target.id,
                    channel_id = %binding.channel_id,
                    kind = kind.as_str(),
                    error = %err,
                    "notifier dispatch failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelConfig, NewNotificationChannel};
    use crate::storage::InMemoryNotificationChannelStore;

    fn test_org() -> OrgId {
        OrgId(Uuid::from_u128(0xA1))
    }

    // ── Pure state machine (the valuable logic) ──────────────────────────

    /// Drive `decide` over a status sequence, collecting emitted events.
    fn run_seq(after: u32, recovery: bool, ups: &[bool]) -> Vec<(AlertKind, u32)> {
        let mut entry = AlertState::default();
        let mut out = Vec::new();
        for &is_up in ups {
            if let (Some(kind), n) = decide(&mut entry, is_up, after, recovery) {
                out.push((kind, n));
            }
        }
        out
    }

    #[test]
    fn fires_once_after_threshold_and_does_not_refire() {
        let ev = run_seq(3, true, &[false, false, false, false, false, false, false]);
        assert_eq!(ev, vec![(AlertKind::Down, 3)]);
    }

    #[test]
    fn emits_recovery_with_failure_count_then_resets() {
        // 4 downs (fires at 2), then up → recovery reports 4, not 0.
        let ev = run_seq(2, true, &[false, false, false, false, true]);
        assert_eq!(ev, vec![(AlertKind::Down, 2), (AlertKind::Recovered, 4)]);
    }

    #[test]
    fn no_recovery_event_when_disabled() {
        let ev = run_seq(2, false, &[false, false, true]);
        assert_eq!(ev, vec![(AlertKind::Down, 2)]);
    }

    #[test]
    fn up_before_threshold_resets_counter() {
        let ev = run_seq(3, true, &[false, false, true, false, false, true]);
        assert!(ev.is_empty());
    }

    #[test]
    fn after_zero_is_floored_to_one() {
        let ev = run_seq(0, true, &[false]);
        assert_eq!(ev, vec![(AlertKind::Down, 1)]);
    }

    // ── Channel resolution + cache ───────────────────────────────────────

    fn engine_with(store: Arc<dyn NotificationChannelStore>) -> AlertEngine {
        let (_tx, rx) = mpsc::channel(4);
        AlertEngine::new(
            rx,
            store,
            crate::http_outbound::build_outbound_client(
                crate::security::SsrfGuard::relaxed_for_tests(),
            ),
        )
    }

    #[tokio::test]
    async fn resolve_returns_none_for_unknown_channel() {
        let store: Arc<dyn NotificationChannelStore> =
            Arc::new(InMemoryNotificationChannelStore::new());
        let engine = engine_with(store);
        assert!(engine.resolve(test_org(), Uuid::now_v7()).await.is_none());
    }

    #[tokio::test]
    async fn resolve_finds_inserted_channel_and_caches_it() {
        let store: Arc<dyn NotificationChannelStore> =
            Arc::new(InMemoryNotificationChannelStore::new());
        let ch = store
            .create(
                test_org(),
                NewNotificationChannel {
                    name: "ops".into(),
                    config: ChannelConfig::Slack {
                        webhook_url: "https://hooks.slack.com/x".into(),
                    },
                    enabled: true,
                },
                10,
            )
            .await
            .unwrap();
        let engine = engine_with(store.clone());
        let got = engine.resolve(test_org(), ch.id).await.expect("resolved");
        assert_eq!(got.name, "ops");
        // Second hit served from cache even after the row is deleted.
        store.delete(test_org(), ch.id).await.unwrap();
        assert!(engine.resolve(test_org(), ch.id).await.is_some());
    }
}
