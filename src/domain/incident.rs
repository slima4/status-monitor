use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

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
