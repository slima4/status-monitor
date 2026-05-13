use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::public::{IncidentSeverity, IncidentStatusPhase, PublicIncidentUpdate};
use super::result::CheckStatus;

/// A contiguous period during which a target was `down` or `error`. Two
/// consecutive bad checks separated by one `up` count as separate incidents.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Incident {
    pub id: Uuid,
    pub target_id: Uuid,
    pub started_at: DateTime<Utc>,
    /// `null` if the incident is ongoing.
    #[schema(nullable = true)]
    pub ended_at: Option<DateTime<Utc>>,
    pub status: CheckStatus,
    /// Duration in seconds. `null` while ongoing.
    #[schema(nullable = true)]
    pub duration_secs: Option<u64>,
    pub check_count: u64,
    #[schema(nullable = true)]
    pub error_sample: Option<String>,
    #[serde(default)]
    #[schema(default = "major")]
    pub severity: IncidentSeverity,
    #[serde(default)]
    #[schema(nullable = true)]
    pub public_title: Option<String>,
    #[serde(default)]
    #[schema(nullable = true)]
    pub public_description: Option<String>,
    #[serde(default)]
    #[schema(nullable = true)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[schema(nullable = true)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updates: Vec<PublicIncidentUpdate>,
}

/// Operator-narration patch.
///
/// `public_title` and `public_description` use a "double-Option" pattern so
/// callers can distinguish three cases:
///  * field omitted entirely → leave the stored value unchanged
///  * field present with JSON `null` → clear the stored value
///  * field present with a string → set the stored value to that string
///
/// utoipa renders both layers as nullable; the wire shape is identical to a
/// plain `Option<String>` to clients.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct IncidentNarrationUpdate {
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<String>)]
    pub public_title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(nullable = true, value_type = Option<String>)]
    pub public_description: Option<Option<String>>,
    #[serde(default)]
    pub severity: Option<IncidentSeverity>,
}

/// Lifts the inner `Option<T>` into a `Some(Option<T>)` so missing fields stay
/// `None` (via `#[serde(default)]`) while explicit JSON `null` becomes
/// `Some(None)` (the "clear" intent). Without this, serde's default treats
/// both as `None`, making it impossible to distinguish "leave alone" from
/// "clear to null" on PATCH.
fn double_option<'de, T, D>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewIncidentUpdate {
    pub phase: IncidentStatusPhase,
    #[schema(min_length = 1, max_length = 2000)]
    pub message: String,
}

/// Coalesces an ordered (ascending by timestamp) stream of check observations
/// into incidents. Each contiguous run of `down`/`error` statuses becomes one
/// `Incident`. Trailing ongoing runs are preserved with `ended_at: None`.
///
/// Callers MUST pass observations sorted by ascending timestamp.
pub fn coalesce_incidents<I>(target_id: Uuid, observations: I) -> Vec<Incident>
where
    I: IntoIterator<Item = (DateTime<Utc>, CheckStatus, Option<String>)>,
{
    let mut out = Vec::new();
    let mut current: Option<Incident> = None;
    for (ts, status, error) in observations {
        let bad = matches!(status, CheckStatus::Down | CheckStatus::Error);
        if bad {
            if let Some(inc) = current.as_mut() {
                inc.check_count += 1;
                if inc.error_sample.is_none() {
                    inc.error_sample = error;
                }
                inc.ended_at = Some(ts);
                inc.duration_secs = Some((ts - inc.started_at).num_seconds().max(0) as u64);
                inc.status = status;
            } else {
                current = Some(Incident {
                    id: Uuid::now_v7(),
                    target_id,
                    started_at: ts,
                    ended_at: None,
                    status,
                    duration_secs: None,
                    check_count: 1,
                    error_sample: error,
                    severity: IncidentSeverity::default(),
                    public_title: None,
                    public_description: None,
                    created_at: None,
                    updated_at: None,
                    updates: Vec::new(),
                });
            }
        } else if let Some(inc) = current.take() {
            out.push(inc);
        }
    }
    if let Some(mut inc) = current {
        // Trailing bad run is still ongoing — clear the `ended_at`/`duration_secs`
        // we may have populated from earlier observations in the same run.
        inc.ended_at = None;
        inc.duration_secs = None;
        out.push(inc);
    }
    out
}
