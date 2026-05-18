//! Per-target alert bindings. A binding references an org-owned
//! [`crate::domain::NotificationChannel`] by id and carries only the
//! per-target firing policy — the transport and its secrets live in
//! `notification_channels`, not here.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AlertBinding {
    /// Id of a channel in the caller's org (`notification_channels.id`).
    #[schema(value_type = String, format = "uuid")]
    pub channel_id: Uuid,
    /// Consecutive non-up checks before a Down alert fires. Must be >= 1.
    #[schema(minimum = 1)]
    pub after_failures: u32,
    #[serde(default = "default_notify_recovery")]
    pub notify_recovery: bool,
}

fn default_notify_recovery() -> bool {
    true
}

/// A target's alert bindings. Serialised transparently as a JSON array so the
/// `targets.alerts` column is a list of `{channel_id, after_failures,
/// notify_recovery}` objects.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(transparent)]
#[schema(value_type = Vec<AlertBinding>)]
pub struct TargetAlerts(pub Vec<AlertBinding>);

impl TargetAlerts {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &AlertBinding> {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_binding_list_with_default_recovery() {
        let json = r#"[{"channel_id":"00000000-0000-0000-0000-000000000001","after_failures":3}]"#;
        let alerts: TargetAlerts = serde_json::from_str(json).unwrap();
        assert_eq!(alerts.iter().count(), 1);
        let b = alerts.0.first().unwrap();
        assert_eq!(b.after_failures, 3);
        assert!(b.notify_recovery);
    }

    #[test]
    fn deserialize_multiple_bindings_and_explicit_recovery() {
        let json = r#"[
            {"channel_id":"00000000-0000-0000-0000-000000000001","after_failures":2,"notify_recovery":false},
            {"channel_id":"00000000-0000-0000-0000-000000000002","after_failures":5}
        ]"#;
        let alerts: TargetAlerts = serde_json::from_str(json).unwrap();
        assert_eq!(alerts.iter().count(), 2);
        assert!(!alerts.0[0].notify_recovery);
        assert!(alerts.0[1].notify_recovery);
    }

    #[test]
    fn empty_default_round_trips_as_array() {
        let alerts = TargetAlerts::default();
        assert!(alerts.is_empty());
        assert_eq!(serde_json::to_string(&alerts).unwrap(), "[]");
    }
}
