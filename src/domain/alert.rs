use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertChannel {
    Slack,
    Webhook,
    Email,
}

impl AlertChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Webhook => "webhook",
            Self::Email => "email",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertChannelConfig {
    pub after_failures: u32,
    #[serde(default = "default_notify_recovery")]
    pub notify_recovery: bool,
    /// Email channel only. `validate_alerts` rejects empty `to` for the email
    /// channel and rejects `to` populated for slack/webhook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
}

fn default_notify_recovery() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct TargetAlerts(pub HashMap<AlertChannel, AlertChannelConfig>);

impl TargetAlerts {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&AlertChannel, &AlertChannelConfig)> {
        self.0.iter()
    }

    pub fn get(&self, channel: AlertChannel) -> Option<&AlertChannelConfig> {
        self.0.get(&channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_slack_webhook_simple_form() {
        let json = r#"{"slack":{"after_failures":3}}"#;
        let alerts: TargetAlerts = serde_json::from_str(json).unwrap();
        let cfg = alerts.get(AlertChannel::Slack).unwrap();
        assert_eq!(cfg.after_failures, 3);
        assert!(cfg.notify_recovery);
        assert!(cfg.to.is_empty());
    }

    #[test]
    fn deserialize_email_with_recipients() {
        let json =
            r#"{"email":{"after_failures":5,"to":["ops@example.com"],"notify_recovery":false}}"#;
        let alerts: TargetAlerts = serde_json::from_str(json).unwrap();
        let cfg = alerts.get(AlertChannel::Email).unwrap();
        assert_eq!(cfg.after_failures, 5);
        assert!(!cfg.notify_recovery);
        assert_eq!(cfg.to, vec!["ops@example.com".to_string()]);
    }

    #[test]
    fn deserialize_multiple_channels() {
        let json = r#"{"slack":{"after_failures":3},"webhook":{"after_failures":6}}"#;
        let alerts: TargetAlerts = serde_json::from_str(json).unwrap();
        assert_eq!(alerts.iter().count(), 2);
    }

    #[test]
    fn empty_default() {
        let alerts = TargetAlerts::default();
        assert!(alerts.is_empty());
    }
}
