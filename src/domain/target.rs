use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::alert::TargetAlerts;
use super::check::CheckSpec;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Target {
    pub id: Uuid,
    pub name: String,
    pub check: CheckSpec,
    /// Check interval in seconds.
    #[serde(with = "duration_secs")]
    #[schema(value_type = u64, example = 60, minimum = 10)]
    pub interval: Duration,
    pub enabled: bool,
    pub tags: Vec<String>,
    #[serde(default)]
    pub alerts: TargetAlerts,
    /// Whether this target appears on the public status page.
    #[serde(default)]
    #[schema(example = false, default = false)]
    pub public_status: bool,
    /// Public display name; falls back to `name` if null.
    #[serde(default)]
    #[schema(example = "API", nullable = true)]
    pub public_name: Option<String>,
    /// Short description shown under the component name on the public page.
    #[serde(default)]
    #[schema(example = "Primary REST endpoint", nullable = true, max_length = 200)]
    pub public_description: Option<String>,
    /// Optional grouping on the public page.
    #[serde(default)]
    #[schema(example = "API", nullable = true, max_length = 50)]
    pub public_group: Option<String>,
    /// Sort order within a group. Lower renders first.
    #[serde(default)]
    #[schema(example = 0, default = 0)]
    pub public_sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NewTarget {
    pub name: String,
    pub check: CheckSpec,
    /// Check interval in seconds.
    #[serde(with = "duration_secs")]
    #[schema(value_type = u64, example = 60, minimum = 10)]
    pub interval: Duration,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub alerts: TargetAlerts,
    #[serde(default)]
    #[schema(example = false, default = false)]
    pub public_status: bool,
    #[serde(default)]
    #[schema(nullable = true)]
    pub public_name: Option<String>,
    #[serde(default)]
    #[schema(nullable = true, max_length = 200)]
    pub public_description: Option<String>,
    #[serde(default)]
    #[schema(nullable = true, max_length = 50)]
    pub public_group: Option<String>,
    #[serde(default)]
    #[schema(default = 0)]
    pub public_sort_order: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct TargetUpdate {
    pub name: Option<String>,
    pub check: Option<CheckSpec>,
    /// Check interval in seconds.
    #[serde(default, with = "duration_secs_opt")]
    #[schema(value_type = Option<u64>)]
    pub interval: Option<Duration>,
    pub enabled: Option<bool>,
    pub tags: Option<Vec<String>>,
    pub alerts: Option<TargetAlerts>,
    pub public_status: Option<bool>,
    // Double-Option so PATCH can tell "field omitted → keep" from "field
    // present as null → clear back to the real monitor name/no group".
    // A plain Option collapses both to None, making un-set impossible.
    // Wire shape is identical to Option<String> for clients.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<String>)]
    pub public_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<String>)]
    pub public_description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<String>)]
    pub public_group: Option<Option<String>>,
    pub public_sort_order: Option<i32>,
}

/// Lifts the inner `Option<T>` into `Some(Option<T>)` so a missing field
/// stays `None` (via `#[serde(default)]` = "leave unchanged") while an
/// explicit JSON `null` becomes `Some(None)` (the "clear" intent).
fn double_option<'de, T, D>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn default_true() -> bool {
    true
}

mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

mod duration_secs_opt {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(v) => s.serialize_some(&v.as_secs()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let secs = Option::<u64>::deserialize(d)?;
        Ok(secs.map(Duration::from_secs))
    }
}
