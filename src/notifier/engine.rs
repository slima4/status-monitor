use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use metrics::counter;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domain::{AlertChannel, CheckStatus};
use crate::notifier::Notifier;
use crate::notifier::event::{AlertEvent, AlertKind, AlertSignal};
use crate::observability::metrics::names;

#[derive(Debug, Default, Clone, Copy)]
struct AlertState {
    consecutive_non_up: u32,
    alerting: bool,
}

pub struct AlertEngine {
    rx: mpsc::Receiver<AlertSignal>,
    notifiers: HashMap<AlertChannel, Arc<dyn Notifier>>,
    state: Arc<Mutex<HashMap<(Uuid, AlertChannel), AlertState>>>,
}

impl AlertEngine {
    pub fn new(rx: mpsc::Receiver<AlertSignal>, notifiers: Vec<Arc<dyn Notifier>>) -> Self {
        let map = notifiers.into_iter().map(|n| (n.channel(), n)).collect();
        Self {
            rx,
            notifiers: map,
            state: Arc::new(Mutex::new(HashMap::new())),
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

    async fn handle(&self, signal: AlertSignal) {
        let target = signal.target;
        if target.alerts.is_empty() {
            return;
        }
        for (channel, cfg) in target.alerts.iter() {
            let Some(notifier) = self.notifiers.get(channel) else {
                tracing::debug!(
                    target_id = %target.id,
                    channel = channel.as_str(),
                    "no globally-enabled notifier for channel; per-target opt-in ignored"
                );
                continue;
            };
            let after = cfg.after_failures.max(1);
            let key = (target.id, *channel);
            let (event_kind, failures_at_event) = {
                let mut guard = self.state.lock();
                let entry = guard.entry(key).or_default();
                let is_up = signal.result.status == CheckStatus::Up;
                match (is_up, entry.alerting) {
                    (true, true) => {
                        let prev_failures = entry.consecutive_non_up;
                        *entry = AlertState::default();
                        let kind = cfg.notify_recovery.then_some(AlertKind::Recovered);
                        (kind, prev_failures)
                    }
                    (true, false) => {
                        entry.consecutive_non_up = 0;
                        (None, 0)
                    }
                    (false, false) => {
                        entry.consecutive_non_up = entry.consecutive_non_up.saturating_add(1);
                        if entry.consecutive_non_up >= after {
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
            };

            let Some(kind) = event_kind else { continue };
            let event = AlertEvent {
                target_id: target.id,
                target_name: target.name.clone(),
                channel: *channel,
                kind,
                consecutive_failures: failures_at_event,
                last_status: signal.result.status,
                last_error: signal.result.error.clone(),
                timestamp: Utc::now(),
                recipients: cfg.to.clone(),
            };
            counter!(
                names::NOTIFICATIONS_TOTAL,
                "channel" => channel.as_str(),
                "kind" => kind.as_str(),
            )
            .increment(1);
            if let Err(err) = notifier.notify(&event).await {
                counter!(
                    names::NOTIFICATIONS_FAILURES,
                    "channel" => channel.as_str(),
                )
                .increment(1);
                tracing::warn!(
                    target_id = %target.id,
                    channel = channel.as_str(),
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
    use crate::domain::{
        AlertChannel, AlertChannelConfig, CheckResult, CheckSpec, CheckStatus, ExpectedStatus,
        HttpCheck, HttpMethod, Target, TargetAlerts,
    };
    use std::collections::HashMap;
    use std::time::Duration;
    use url::Url;

    struct StubNotifier {
        channel: AlertChannel,
        received: Arc<Mutex<Vec<AlertEvent>>>,
    }

    #[async_trait::async_trait]
    impl Notifier for StubNotifier {
        fn channel(&self) -> AlertChannel {
            self.channel
        }
        async fn notify(&self, event: &AlertEvent) -> crate::error::Result<()> {
            self.received.lock().push(event.clone());
            Ok(())
        }
    }

    fn make_target(alerts: TargetAlerts) -> Arc<Target> {
        let url = Url::parse("https://example.com/").unwrap();
        Arc::new(Target {
            id: Uuid::now_v7(),
            name: "test".into(),
            check: CheckSpec::Http(HttpCheck {
                url,
                method: HttpMethod::Get,
                timeout: Duration::from_secs(5),
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
            interval: Duration::from_secs(60),
            enabled: true,
            tags: vec![],
            alerts,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    fn result(status: CheckStatus, target_id: Uuid) -> CheckResult {
        CheckResult {
            target_id,
            timestamp: Utc::now(),
            status,
            duration_ms: 1,
            dns_ms: None,
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: None,
        }
    }

    fn build_engine(channel: AlertChannel) -> (AlertEngine, Arc<Mutex<Vec<AlertEvent>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let notifier = Arc::new(StubNotifier {
            channel,
            received: received.clone(),
        });
        let (_tx, rx) = mpsc::channel(16);
        let engine = AlertEngine::new(rx, vec![notifier]);
        (engine, received)
    }

    fn alerts_with(channel: AlertChannel, after: u32, recovery: bool) -> TargetAlerts {
        let mut map = HashMap::new();
        map.insert(
            channel,
            AlertChannelConfig {
                after_failures: after,
                notify_recovery: recovery,
                to: vec![],
            },
        );
        TargetAlerts(map)
    }

    #[tokio::test]
    async fn fires_after_threshold() {
        let (engine, received) = build_engine(AlertChannel::Slack);
        let target = make_target(alerts_with(AlertChannel::Slack, 3, true));
        for _ in 0..3 {
            engine
                .handle(AlertSignal {
                    target: target.clone(),
                    result: result(CheckStatus::Down, target.id),
                })
                .await;
        }
        assert_eq!(received.lock().len(), 1);
        assert_eq!(received.lock()[0].kind, AlertKind::Down);
    }

    #[tokio::test]
    async fn does_not_refire_while_alerting() {
        let (engine, received) = build_engine(AlertChannel::Slack);
        let target = make_target(alerts_with(AlertChannel::Slack, 3, true));
        for _ in 0..7 {
            engine
                .handle(AlertSignal {
                    target: target.clone(),
                    result: result(CheckStatus::Down, target.id),
                })
                .await;
        }
        assert_eq!(received.lock().len(), 1);
    }

    #[tokio::test]
    async fn emits_recovery_after_alert() {
        let (engine, received) = build_engine(AlertChannel::Slack);
        let target = make_target(alerts_with(AlertChannel::Slack, 2, true));
        for _ in 0..4 {
            engine
                .handle(AlertSignal {
                    target: target.clone(),
                    result: result(CheckStatus::Down, target.id),
                })
                .await;
        }
        engine
            .handle(AlertSignal {
                target: target.clone(),
                result: result(CheckStatus::Up, target.id),
            })
            .await;
        let events = received.lock().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, AlertKind::Down);
        assert_eq!(events[0].consecutive_failures, 2);
        assert_eq!(events[1].kind, AlertKind::Recovered);
        // Recovery event should report the failure count at which the alert fired,
        // not zero — protects against a regression where `mem::take` clears the
        // counter before the event is constructed.
        assert_eq!(events[1].consecutive_failures, 4);
    }

    #[tokio::test]
    async fn no_recovery_when_disabled() {
        let (engine, received) = build_engine(AlertChannel::Slack);
        let target = make_target(alerts_with(AlertChannel::Slack, 2, false));
        for _ in 0..2 {
            engine
                .handle(AlertSignal {
                    target: target.clone(),
                    result: result(CheckStatus::Down, target.id),
                })
                .await;
        }
        engine
            .handle(AlertSignal {
                target: target.clone(),
                result: result(CheckStatus::Up, target.id),
            })
            .await;
        assert_eq!(received.lock().len(), 1);
        assert_eq!(received.lock()[0].kind, AlertKind::Down);
    }

    #[tokio::test]
    async fn resets_counter_on_up() {
        let (engine, received) = build_engine(AlertChannel::Slack);
        let target = make_target(alerts_with(AlertChannel::Slack, 3, true));
        let sequence = [
            CheckStatus::Down,
            CheckStatus::Down,
            CheckStatus::Up,
            CheckStatus::Down,
            CheckStatus::Down,
            CheckStatus::Up,
        ];
        for s in sequence {
            engine
                .handle(AlertSignal {
                    target: target.clone(),
                    result: result(s, target.id),
                })
                .await;
        }
        assert!(received.lock().is_empty());
    }

    #[tokio::test]
    async fn isolates_per_channel() {
        let received_slack = Arc::new(Mutex::new(Vec::new()));
        let received_web = Arc::new(Mutex::new(Vec::new()));
        let slack = Arc::new(StubNotifier {
            channel: AlertChannel::Slack,
            received: received_slack.clone(),
        });
        let web = Arc::new(StubNotifier {
            channel: AlertChannel::Webhook,
            received: received_web.clone(),
        });
        let (_tx, rx) = mpsc::channel(16);
        let engine = AlertEngine::new(rx, vec![slack, web]);
        let mut map = HashMap::new();
        map.insert(
            AlertChannel::Slack,
            AlertChannelConfig {
                after_failures: 2,
                notify_recovery: true,
                to: vec![],
            },
        );
        map.insert(
            AlertChannel::Webhook,
            AlertChannelConfig {
                after_failures: 4,
                notify_recovery: true,
                to: vec![],
            },
        );
        let target = make_target(TargetAlerts(map));
        for _ in 0..2 {
            engine
                .handle(AlertSignal {
                    target: target.clone(),
                    result: result(CheckStatus::Down, target.id),
                })
                .await;
        }
        assert_eq!(received_slack.lock().len(), 1);
        assert!(received_web.lock().is_empty());
    }

    #[tokio::test]
    async fn unknown_channel_is_no_op() {
        let (engine, received) = build_engine(AlertChannel::Slack);
        let target = make_target(alerts_with(AlertChannel::Email, 1, true));
        engine
            .handle(AlertSignal {
                target: target.clone(),
                result: result(CheckStatus::Down, target.id),
            })
            .await;
        assert!(received.lock().is_empty());
    }
}
