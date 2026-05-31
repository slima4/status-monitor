use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use metrics::counter;
use moka::sync::Cache;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
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

/// Idle !alerting entries past this are orphans (deleted target/channel/
/// binding). Latched (alerting=true) entries are never swept — re-pageing a
/// continuously-down target after eviction would be worse than the memory.
const STATE_TTL: Duration = Duration::from_secs(2 * 3600);

const STATE_SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy)]
struct AlertState {
    consecutive_non_up: u32,
    alerting: bool,
    last_touched: Instant,
}

impl Default for AlertState {
    fn default() -> Self {
        Self {
            consecutive_non_up: 0,
            alerting: false,
            last_touched: Instant::now(),
        }
    }
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

/// Resolved-channel cache shared between the [`AlertEngine`] (reader) and
/// the API handlers (invalidator). Cheaply cloned — moka wraps the inner
/// state in an Arc — so both sides see the same entries. Without explicit
/// invalidation on edit/delete, a webhook URL change or a revoked Slack
/// token would still ship to the old endpoint until the TTL elapsed.
#[derive(Clone)]
pub struct AlertChannelCache {
    inner: Cache<(OrgId, Uuid), Option<Arc<NotificationChannel>>>,
}

impl AlertChannelCache {
    pub fn new() -> Self {
        Self {
            inner: Cache::builder()
                .time_to_live(CHANNEL_CACHE_TTL)
                .max_capacity(4096)
                .build(),
        }
    }

    pub fn invalidate(&self, org: OrgId, id: Uuid) {
        self.inner.invalidate(&(org, id));
    }

    fn get(&self, key: &(OrgId, Uuid)) -> Option<Option<Arc<NotificationChannel>>> {
        self.inner.get(key)
    }

    fn insert(&self, key: (OrgId, Uuid), val: Option<Arc<NotificationChannel>>) {
        self.inner.insert(key, val);
    }
}

impl Default for AlertChannelCache {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AlertEngine {
    rx: mpsc::Receiver<AlertSignal>,
    channels: Arc<dyn NotificationChannelStore>,
    http: crate::http_outbound::OutboundHttpClient,
    /// `handle()` is the sole writer (serial consumer of `rx`); the rollback
    /// at the bottom relies on this — parallelizing per-binding fan-out will
    /// break the prev_state restore.
    state: Arc<Mutex<HashMap<(Uuid, Uuid), AlertState>>>,
    /// Keyed by `(org, channel_id)` so a tenant's binding can only ever hit
    /// that tenant's channel. `None` is cached too, so a binding to a deleted
    /// channel doesn't hammer the store on every check.
    cache: AlertChannelCache,
}

impl AlertEngine {
    pub fn new(
        rx: mpsc::Receiver<AlertSignal>,
        channels: Arc<dyn NotificationChannelStore>,
        http: crate::http_outbound::OutboundHttpClient,
        cache: AlertChannelCache,
    ) -> Self {
        Self {
            rx,
            channels,
            http,
            state: Arc::new(Mutex::new(HashMap::new())),
            cache,
        }
    }

    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut sweep = tokio::time::interval(STATE_SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                maybe = self.rx.recv() => {
                    match maybe {
                        Some(signal) => self.handle(signal).await,
                        None => return,
                    }
                }
                _ = sweep.tick() => {
                    let evicted = self.sweep_idle(STATE_TTL);
                    if evicted > 0 {
                        tracing::debug!(evicted, "alert engine state swept");
                    }
                }
            }
        }
    }

    fn sweep_idle(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let mut guard = self.state.lock();
        let before = guard.len();
        guard.retain(|_k, v| v.alerting || now.duration_since(v.last_touched) < ttl);
        before - guard.len()
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
            let key = (target.id, binding.channel_id);
            let touch_at = Instant::now();
            let (event_kind, failures_at_event, prev_state) = {
                let mut guard = self.state.lock();
                let entry = guard.entry(key).or_default();
                let prev = *entry;
                let (k, n) = decide(
                    entry,
                    is_up,
                    binding.after_failures,
                    binding.notify_recovery,
                );
                entry.last_touched = touch_at;
                (k, n, prev)
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
            // Rollback on dispatch failure — without it, a failed delivery
            // latches alerting=true and silently suppresses every future Down.
            let dispatch_ok = match build_notifier(&channel.config, &self.http) {
                Ok(n) => match n.notify(&event).await {
                    Ok(()) => true,
                    Err(err) => {
                        tracing::warn!(
                            target_id = %target.id,
                            channel_id = %binding.channel_id,
                            kind = kind.as_str(),
                            error = %err,
                            "notifier dispatch failed"
                        );
                        false
                    }
                },
                Err(err) => {
                    tracing::warn!(
                        target_id = %target.id,
                        channel_id = %binding.channel_id,
                        kind = kind.as_str(),
                        error = %err,
                        "building notifier failed"
                    );
                    false
                }
            };
            if !dispatch_ok {
                counter!(names::NOTIFICATIONS_FAILURES, "channel" => channel_label).increment(1);
                let mut restored = prev_state;
                restored.last_touched = touch_at;
                self.state.lock().insert(key, restored);
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
            AlertChannelCache::new(),
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
                crate::domain::WriteSource::Ui,
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

    #[tokio::test]
    async fn cache_invalidate_drops_the_entry_so_next_resolve_re_reads() {
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
                crate::domain::WriteSource::Ui,
                10,
            )
            .await
            .unwrap();
        let cache = AlertChannelCache::new();
        let (_tx, rx) = mpsc::channel(4);
        let engine = AlertEngine::new(
            rx,
            store.clone(),
            crate::http_outbound::build_outbound_client(
                crate::security::SsrfGuard::relaxed_for_tests(),
            ),
            cache.clone(),
        );
        assert!(engine.resolve(test_org(), ch.id).await.is_some());
        store.delete(test_org(), ch.id).await.unwrap();
        cache.invalidate(test_org(), ch.id);
        assert!(
            engine.resolve(test_org(), ch.id).await.is_none(),
            "post-invalidate resolve must re-read the store"
        );
    }

    // ── Dispatch-failure rollback (FIRE-LOSS regression) ─────────────────

    #[tokio::test]
    async fn dispatch_failure_rolls_back_alerting_so_next_signal_retries() {
        use crate::domain::{
            AlertBinding, CheckResult, CheckSpec, CheckStatus, ExpectedStatus, HttpCheck,
            HttpMethod, NewNotificationChannel, Target, TargetAlerts,
        };
        use chrono::Utc;
        use std::collections::HashMap;
        use std::time::Duration as StdDuration;

        // Closed loopback port → dispatch fails at TCP connect, no mock server.
        let store: Arc<dyn NotificationChannelStore> =
            Arc::new(InMemoryNotificationChannelStore::new());
        let ch = store
            .create(
                test_org(),
                NewNotificationChannel {
                    name: "ops".into(),
                    config: ChannelConfig::Webhook {
                        url: "http://127.0.0.1:1/notify".into(),
                        headers: Default::default(),
                    },
                    enabled: true,
                },
                crate::domain::WriteSource::Ui,
                10,
            )
            .await
            .unwrap();

        let engine = engine_with(store);

        let target = Arc::new(Target {
            id: Uuid::now_v7(),
            name: "boom".into(),
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
                channel_id: ch.id,
                after_failures: 1,
                notify_recovery: true,
            }]),
            group_name: None,
            owner_user_id: None,
            public_status: false,
            public_name: None,
            public_description: None,
            public_group: None,
            public_sort_order: 0,
            write_source: crate::domain::WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });

        let down_signal = || AlertSignal {
            target: target.clone(),
            org_id: test_org(),
            result: CheckResult {
                target_id: target.id,
                org_id: test_org().0,
                timestamp: Utc::now(),
                status: CheckStatus::Down,
                duration_ms: 0,
                dns_ms: None,
                connect_ms: None,
                tls_ms: None,
                ttfb_ms: None,
                response_code: None,
                response_size: None,
                error: Some("test outage".into()),
            },
        };

        let key = (target.id, ch.id);

        engine.handle(down_signal()).await;
        let after_first = *engine.state.lock().get(&key).expect("state inserted");
        assert!(
            !after_first.alerting,
            "alerting must roll back after failed dispatch"
        );

        engine.handle(down_signal()).await;
        let after_second = *engine.state.lock().get(&key).unwrap();
        assert!(
            !after_second.alerting,
            "second failure must also roll back, proving retry"
        );
    }

    #[tokio::test]
    async fn recovered_dispatch_failure_keeps_alerting_so_recovery_retries() {
        // (true,true) resets state on success — failed Recovered must keep
        // alerting=true so the next Up retries instead of dropping the event.
        use crate::domain::{
            AlertBinding, CheckResult, CheckSpec, CheckStatus, ExpectedStatus, HttpCheck,
            HttpMethod, NewNotificationChannel, Target, TargetAlerts,
        };
        use chrono::Utc;
        use std::collections::HashMap;
        use std::time::Duration as StdDuration;

        let store: Arc<dyn NotificationChannelStore> =
            Arc::new(InMemoryNotificationChannelStore::new());
        let ch = store
            .create(
                test_org(),
                NewNotificationChannel {
                    name: "ops".into(),
                    config: ChannelConfig::Webhook {
                        url: "http://127.0.0.1:1/notify".into(),
                        headers: Default::default(),
                    },
                    enabled: true,
                },
                crate::domain::WriteSource::Ui,
                10,
            )
            .await
            .unwrap();

        let engine = engine_with(store);

        let target = Arc::new(Target {
            id: Uuid::now_v7(),
            name: "fluky".into(),
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
                channel_id: ch.id,
                after_failures: 1,
                notify_recovery: true,
            }]),
            group_name: None,
            owner_user_id: None,
            public_status: false,
            public_name: None,
            public_description: None,
            public_group: None,
            public_sort_order: 0,
            write_source: crate::domain::WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });

        let key = (target.id, ch.id);
        engine.state.lock().insert(
            key,
            AlertState {
                consecutive_non_up: 4,
                alerting: true,
                last_touched: Instant::now(),
            },
        );

        let up_signal = AlertSignal {
            target: target.clone(),
            org_id: test_org(),
            result: CheckResult {
                target_id: target.id,
                org_id: test_org().0,
                timestamp: Utc::now(),
                status: CheckStatus::Up,
                duration_ms: 10,
                dns_ms: None,
                connect_ms: None,
                tls_ms: None,
                ttfb_ms: None,
                response_code: Some(200),
                response_size: Some(0),
                error: None,
            },
        };

        engine.handle(up_signal).await;
        let after = *engine.state.lock().get(&key).unwrap();
        assert!(
            after.alerting,
            "alerting must persist across failed Recovered"
        );
        assert_eq!(
            after.consecutive_non_up, 4,
            "rollback must preserve the failure count for the eventual Recovered payload",
        );
    }

    #[tokio::test]
    async fn sweep_evicts_idle_non_alerting_and_keeps_fresh_and_latched() {
        let store: Arc<dyn NotificationChannelStore> =
            Arc::new(InMemoryNotificationChannelStore::new());
        let engine = engine_with(store);

        let baseline = Instant::now();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let fresh = (Uuid::now_v7(), Uuid::now_v7());
        let stale_orphan = (Uuid::now_v7(), Uuid::now_v7());
        let stale_latched = (Uuid::now_v7(), Uuid::now_v7());
        {
            let mut guard = engine.state.lock();
            guard.insert(fresh, AlertState::default());
            guard.insert(
                stale_orphan,
                AlertState {
                    consecutive_non_up: 1,
                    alerting: false,
                    last_touched: baseline,
                },
            );
            guard.insert(
                stale_latched,
                AlertState {
                    consecutive_non_up: 7,
                    alerting: true,
                    last_touched: baseline,
                },
            );
        }

        let evicted = engine.sweep_idle(Duration::from_millis(10));
        assert_eq!(
            evicted, 1,
            "only the idle non-alerting entry must be reclaimed"
        );
        let guard = engine.state.lock();
        assert!(guard.contains_key(&fresh), "fresh entry must survive");
        assert!(
            !guard.contains_key(&stale_orphan),
            "stale orphan must be evicted"
        );
        assert!(
            guard.contains_key(&stale_latched),
            "latched entry must never be swept — sweeping would let the next \
             Down re-fire and double-page on a continuously-down target",
        );
    }

    #[tokio::test]
    async fn handle_refreshes_last_touched_so_active_bindings_survive_sweep() {
        use crate::domain::{
            AlertBinding, CheckResult, CheckSpec, CheckStatus, ExpectedStatus, HttpCheck,
            HttpMethod, NewNotificationChannel, Target, TargetAlerts,
        };
        use chrono::Utc;
        use std::collections::HashMap;
        use std::time::Duration as StdDuration;

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
                crate::domain::WriteSource::Ui,
                10,
            )
            .await
            .unwrap();

        let engine = engine_with(store);

        let target = Arc::new(Target {
            id: Uuid::now_v7(),
            name: "live".into(),
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
                channel_id: ch.id,
                after_failures: 5,
                notify_recovery: true,
            }]),
            group_name: None,
            owner_user_id: None,
            public_status: false,
            public_name: None,
            public_description: None,
            public_group: None,
            public_sort_order: 0,
            write_source: crate::domain::WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });

        let key = (target.id, ch.id);
        let baseline = Instant::now();
        tokio::time::sleep(Duration::from_millis(50)).await;
        engine.state.lock().insert(
            key,
            AlertState {
                consecutive_non_up: 0,
                alerting: false,
                last_touched: baseline,
            },
        );

        let up_signal = AlertSignal {
            target: target.clone(),
            org_id: test_org(),
            result: CheckResult {
                target_id: target.id,
                org_id: test_org().0,
                timestamp: Utc::now(),
                status: CheckStatus::Up,
                duration_ms: 5,
                dns_ms: None,
                connect_ms: None,
                tls_ms: None,
                ttfb_ms: None,
                response_code: Some(200),
                response_size: Some(0),
                error: None,
            },
        };
        engine.handle(up_signal).await;

        let evicted = engine.sweep_idle(Duration::from_millis(10));
        assert_eq!(evicted, 0, "refreshed entry must not be swept");
        assert!(engine.state.lock().contains_key(&key));
    }
}
