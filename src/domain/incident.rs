use chrono::{DateTime, Duration as ChronoDuration, Utc};
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

impl Incident {
    /// Resolved duration of a CLOSED incident. Returns `None` for ongoing
    /// incidents. Prefers the stored `duration_secs` set by the coalescer;
    /// falls back to `(ended_at - started_at)` when missing. Saturating
    /// cast guards against `u64::MAX` overflowing `i64` (unreachable in
    /// practice; the duration would have to exceed ~292 billion years).
    pub fn closed_duration(&self) -> Option<ChronoDuration> {
        self.ended_at.map(|end| {
            if let Some(s) = self.duration_secs {
                return ChronoDuration::seconds(i64::try_from(s).unwrap_or(i64::MAX));
            }
            (end - self.started_at).max(ChronoDuration::zero())
        })
    }

    /// Replace the error sample with its customer-safe form (the internal
    /// `served_stale:` annotation removed). Must be called before any
    /// customer-facing serialization of this incident.
    pub fn sanitize_error_sample(&mut self) {
        if let Some(e) = self.error_sample.take() {
            self.error_sample = crate::domain::strip_served_stale(&e).map(str::to_owned);
        }
    }
}

/// Wall-clock duration of an incident at the given `now`. Open-ended
/// incidents clamp to `now`. Used by view layers that want a single
/// "how long has this been going on" number for both closed and
/// ongoing rows. Always non-negative.
pub fn elapsed_at(
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> ChronoDuration {
    let end = ended_at.unwrap_or(now);
    (end - started_at).max(ChronoDuration::zero())
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
                extend_incident(inc, ts, status, error);
            } else {
                current = Some(new_incident(target_id, ts, status, error));
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

/// Same shape as [`coalesce_incidents`] but assumes the input has been
/// pre-filtered to `down`/`error` observations at the storage layer
/// (the SQL `AND status IN ('down','error')` path). Without `up`/`degraded`
/// markers, recovery is inferred two ways:
///
///   - **Mid-stream gap.** Two consecutive bad observations more than
///     `recovery_threshold` apart split into two incidents; the earlier
///     one is closed at its last seen `ts` (tightest bound we have).
///   - **Trailing gap.** If the *last* bad observation is more than
///     `recovery_threshold` before `range_end`, the run is closed at
///     that last `ts`; otherwise the run is reported as ongoing
///     (`ended_at: None`). This avoids surfacing a stale "Ongoing" badge
///     three hours after the monitor actually recovered just because the
///     recovery row was filtered out at SQL.
///
/// Callers MUST pass observations sorted by ascending timestamp.
pub fn coalesce_incidents_bad_only<I>(
    target_id: Uuid,
    observations: I,
    range_end: DateTime<Utc>,
    recovery_threshold: ChronoDuration,
) -> Vec<Incident>
where
    I: IntoIterator<Item = (DateTime<Utc>, CheckStatus, Option<String>)>,
{
    let mut out = Vec::new();
    let mut current: Option<(Incident, DateTime<Utc>)> = None;
    for (ts, status, error) in observations {
        if !matches!(status, CheckStatus::Down | CheckStatus::Error) {
            continue;
        }
        if let Some((mut inc, last_ts)) = current.take() {
            if ts - last_ts > recovery_threshold {
                close_at(&mut inc, last_ts);
                out.push(inc);
                current = Some((new_incident(target_id, ts, status, error), ts));
            } else {
                extend_incident(&mut inc, ts, status, error);
                current = Some((inc, ts));
            }
        } else {
            current = Some((new_incident(target_id, ts, status, error), ts));
        }
    }
    if let Some((mut inc, last_ts)) = current {
        if range_end - last_ts > recovery_threshold {
            // Last bad sighting is far behind the window's edge → recovery
            // landed in the trailing gap and got filtered out.
            close_at(&mut inc, last_ts);
        } else {
            inc.ended_at = None;
            inc.duration_secs = None;
        }
        out.push(inc);
    }
    out
}

/// Pins a run's close to `last_ts` (the tightest bound we have). Covers
/// the single-bad-row case where `extend_incident` never ran and
/// `ended_at` is still `None` from `new_incident`.
fn close_at(inc: &mut Incident, last_ts: DateTime<Utc>) {
    inc.ended_at = Some(last_ts);
    inc.duration_secs = Some((last_ts - inc.started_at).num_seconds().max(0) as u64);
}

fn extend_incident(
    inc: &mut Incident,
    ts: DateTime<Utc>,
    status: CheckStatus,
    error: Option<String>,
) {
    inc.check_count += 1;
    if inc.error_sample.is_none() {
        inc.error_sample = error;
    }
    inc.ended_at = Some(ts);
    inc.duration_secs = Some((ts - inc.started_at).num_seconds().max(0) as u64);
    inc.status = status;
}

fn new_incident(
    target_id: Uuid,
    ts: DateTime<Utc>,
    status: CheckStatus,
    error: Option<String>,
) -> Incident {
    Incident {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(seconds_from_epoch: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds_from_epoch, 0).single().unwrap()
    }

    fn bad(s: i64) -> (DateTime<Utc>, CheckStatus, Option<String>) {
        (ts(s), CheckStatus::Down, Some("boom".into()))
    }

    const THRESHOLD: ChronoDuration = ChronoDuration::seconds(120);

    #[test]
    fn bad_only_trailing_run_within_threshold_is_ongoing() {
        let inc = coalesce_incidents_bad_only(
            Uuid::nil(),
            vec![bad(60), bad(120), bad(180)],
            ts(200), // range_end only 20s past last bad → within threshold
            THRESHOLD,
        );
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].started_at, ts(60));
        assert_eq!(inc[0].check_count, 3);
        assert!(inc[0].ended_at.is_none());
        assert!(inc[0].duration_secs.is_none());
    }

    #[test]
    fn bad_only_trailing_run_past_threshold_is_closed() {
        let inc = coalesce_incidents_bad_only(
            Uuid::nil(),
            vec![bad(60), bad(120), bad(180)],
            ts(600), // range_end 420s past last bad → far past threshold
            THRESHOLD,
        );
        assert_eq!(inc.len(), 1);
        // Closed at the last seen bad ts (tightest bound).
        assert_eq!(inc[0].ended_at, Some(ts(180)));
        assert_eq!(inc[0].duration_secs, Some(120));
    }

    #[test]
    fn bad_only_gap_larger_than_threshold_splits_into_two() {
        let inc = coalesce_incidents_bad_only(
            Uuid::nil(),
            vec![bad(60), bad(120), bad(600), bad(660)],
            ts(700), // last bad is recent → trailing run ongoing
            THRESHOLD,
        );
        assert_eq!(inc.len(), 2);
        // First run closed at its last bad ts (mid-stream gap detected).
        assert_eq!(inc[0].started_at, ts(60));
        assert_eq!(inc[0].ended_at, Some(ts(120)));
        // Second run still ongoing within window.
        assert_eq!(inc[1].started_at, ts(600));
        assert!(inc[1].ended_at.is_none());
    }

    #[test]
    fn bad_only_ignores_non_bad_observations_defensively() {
        // Caller is supposed to filter at SQL, but defend against a slip.
        let inc = coalesce_incidents_bad_only(
            Uuid::nil(),
            vec![
                (ts(60), CheckStatus::Up, None),
                bad(120),
                (ts(180), CheckStatus::Degraded, None),
                bad(240),
            ],
            ts(300),
            THRESHOLD,
        );
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].started_at, ts(120));
    }

    #[test]
    fn bad_only_gap_exactly_at_threshold_keeps_run() {
        let inc =
            coalesce_incidents_bad_only(Uuid::nil(), vec![bad(60), bad(180)], ts(200), THRESHOLD);
        // gap == threshold → not greater than, still same run
        assert_eq!(inc.len(), 1);
    }

    #[test]
    fn bad_only_trailing_gap_exactly_at_threshold_stays_ongoing() {
        // range_end - last_ts == THRESHOLD → not strictly greater, still ongoing.
        let inc =
            coalesce_incidents_bad_only(Uuid::nil(), vec![bad(60), bad(120)], ts(240), THRESHOLD);
        assert_eq!(inc.len(), 1);
        assert!(inc[0].ended_at.is_none());
        assert!(inc[0].duration_secs.is_none());
    }

    #[test]
    fn bad_only_isolated_single_bad_rows_split_into_closed_incidents() {
        // A flappy monitor: isolated single-bad observations spaced > threshold.
        // Pre-fix bug: the split-off run kept `ended_at = None` from
        // `new_incident` (loop's extend never ran), so each fired as
        // "ongoing", ballooning the badge.
        let inc = coalesce_incidents_bad_only(
            Uuid::nil(),
            vec![bad(60), bad(600), bad(1200)],
            ts(1250), // trailing run still within threshold → ongoing
            THRESHOLD,
        );
        assert_eq!(inc.len(), 3);
        // First two are isolated singles → must be closed at their own ts.
        assert_eq!(inc[0].ended_at, Some(ts(60)));
        assert_eq!(inc[0].duration_secs, Some(0));
        assert_eq!(inc[1].ended_at, Some(ts(600)));
        assert_eq!(inc[1].duration_secs, Some(0));
        // Only the trailing run is ongoing.
        assert!(inc[2].ended_at.is_none());
    }

    #[test]
    fn bad_only_single_row_trailing_past_threshold_closes_at_that_row() {
        // Single bad observation, range_end way past threshold. Old code
        // pushed with ended_at=None because the mid-loop never set it.
        let inc = coalesce_incidents_bad_only(Uuid::nil(), vec![bad(60)], ts(600), THRESHOLD);
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].ended_at, Some(ts(60)));
        assert_eq!(inc[0].duration_secs, Some(0));
        assert_eq!(inc[0].check_count, 1);
    }
}
