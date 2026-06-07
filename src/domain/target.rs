use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::WriteSource;
use super::alert::TargetAlerts;
use super::check::CheckSpec;

/// How a target's per-region health folds into incidents. Stored per monitor;
/// the incident writer's `decide_multi` consumes it. `AnyDown` and `Quorum` keep
/// one whole-target incident; `PerRegion` is reserved (not yet selectable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum RegionIncidentPolicy {
    /// Open as soon as any region is sustained-bad.
    #[default]
    AnyDown,
    /// Open once at least `n` regions agree it is down (location quorum).
    Quorum(u32),
    /// One incident per region down.
    PerRegion,
}

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
    /// How multi-region health folds into incidents for this monitor.
    #[serde(default)]
    pub region_policy: RegionIncidentPolicy,
    /// Operator-side grouping (independent of any status page's grouping).
    #[serde(default)]
    #[schema(example = "API & Web", nullable = true, max_length = 50)]
    pub group_name: Option<String>,
    /// FK to `users.id`. Nullable; cleared if the user is removed.
    #[serde(default)]
    #[schema(nullable = true)]
    pub owner_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Where this target was last changed from (UI, API, or Terraform).
    #[serde(default)]
    pub write_source: WriteSource,
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
    pub region_policy: RegionIncidentPolicy,
    #[serde(default)]
    #[schema(nullable = true, max_length = 50)]
    pub group_name: Option<String>,
    #[serde(default)]
    #[schema(nullable = true)]
    pub owner_user_id: Option<Uuid>,
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
    #[serde(default)]
    pub region_policy: Option<RegionIncidentPolicy>,
    pub tags: Option<Vec<String>>,
    pub alerts: Option<TargetAlerts>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<String>)]
    pub group_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<Uuid>)]
    pub owner_user_id: Option<Option<Uuid>>,
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
