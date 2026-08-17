//! The pure decision: what the recent results say should open or close, with
//! no database and no clock of its own.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus};

use super::{NewOpenIncident, OpenIncident};

/// Decision produced by [`decide`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Open(NewOpenIncident),
    Close {
        incident_id: Uuid,
        ended_at: DateTime<Utc>,
    },
}

struct Verdict<'a> {
    region: &'a str,
    bad: &'a [CheckResult],
    good: &'a [CheckResult],
}

/// Single-region convenience over [`decide_multi`]: one region, combined
/// any-down policy. Kept for call sites and tests that reason about a flat
/// result stream. Returns at most one [`Action`].
///
/// **Idempotency**: referentially transparent. Any `Action::Open` it returns
/// assumes the caller has just verified there is no open incident — re-running
/// after the write falls through to `Action::None`.
pub fn decide(open: Option<&OpenIncident>, results: &[CheckResult], flap_threshold: u32) -> Action {
    let Some(target_id) = results.first().map(|r| r.target_id) else {
        return Action::None;
    };
    let opens: Vec<OpenIncident> = open.cloned().into_iter().collect();
    let by_region = [(String::new(), results.to_vec())];
    decide_multi(target_id, &opens, &by_region, flap_threshold, 1)
        .into_iter()
        .next()
        .unwrap_or(Action::None)
}

/// Pure region-aware decision. Each `(region, results)` group is one region's
/// checks ascending by time; `opens` is every open incident for the target.
/// `confirmations` is the per-region consecutive-bad run needed; `quorum` is how
/// many regions must agree before the combined incident opens (clamped to the
/// live region count so it can never be unreachable). Returns the writes to
/// apply; an empty vec means nothing to do.
pub fn decide_multi(
    target_id: Uuid,
    opens: &[OpenIncident],
    by_region: &[(String, Vec<CheckResult>)],
    confirmations: u32,
    quorum: usize,
) -> Vec<Action> {
    let threshold = (confirmations as usize).max(1);

    let verdicts: Vec<Verdict> = by_region
        .iter()
        .map(|(region, results)| Verdict {
            region,
            bad: trailing_bad_run(results),
            good: trailing_good_run(results),
        })
        .collect();

    let quorum = quorum.clamp(1, verdicts.len().max(1));
    let mut bad: Vec<&Verdict> = verdicts
        .iter()
        .filter(|v| v.bad.len() >= threshold)
        .collect();
    bad.sort_by_key(|v| v.bad[0].timestamp);
    let combined = opens.iter().find(|i| i.region.is_none());

    match combined {
        None => {
            if bad.len() >= quorum {
                // region = None: one whole-target incident, so its key must be
                // region-independent or the next tick re-opens it.
                let trigger = bad[quorum - 1];
                let origin = bad[0];
                // Worst across every confirmed bad run — a degraded origin
                // region must not mask a concurrently hard-down region.
                let status_at_start = bad
                    .iter()
                    .flat_map(|v| v.bad.iter().map(|r| r.status))
                    .max_by_key(|s| severity_rank(*s))
                    .unwrap_or(CheckStatus::Down);
                vec![Action::Open(NewOpenIncident {
                    target_id,
                    started_at: trigger.bad[0].timestamp,
                    status_at_start,
                    check_count: origin.bad.len() as u32,
                    error_sample: incident_error_sample(&bad, verdicts.len(), quorum),
                    region: None,
                    regions_down: bad
                        .iter()
                        .map(|v| v.region)
                        .filter(|r| !r.is_empty())
                        .map(str::to_string)
                        .collect(),
                    regions_up: verdicts
                        .iter()
                        .filter(|v| v.bad.len() < threshold && !v.region.is_empty())
                        .map(|v| v.region.to_string())
                        .collect(),
                })]
            } else {
                vec![]
            }
        }
        Some(inc) => {
            // Close once below quorum, with a sustained-good region as recovery
            // evidence; ended_at = latest such recovery.
            if bad.len() < quorum {
                let ended = verdicts
                    .iter()
                    .filter(|v| v.good.len() >= threshold)
                    .map(|v| v.good[0].timestamp)
                    .max();
                if let Some(ended) = ended
                    && ended > inc.started_at
                {
                    return vec![Action::Close {
                        incident_id: inc.id,
                        ended_at: ended,
                    }];
                }
            }
            vec![]
        }
    }
}

/// Cause as stated to notifications and incident views. The per-result error
/// is left untouched for API consumers.
fn incident_error_sample(
    bad: &[&Verdict<'_>],
    reporting_regions: usize,
    quorum: usize,
) -> Option<String> {
    // Newest failure per region only: an earlier page must not outlive a
    // change in how the edge is failing.
    let diagnosed: Vec<_> = bad
        .iter()
        .filter_map(|verdict| {
            let result = verdict.bad.last()?;
            result
                .diagnostic
                .as_ref()
                .map(|diagnostic| (result, diagnostic))
        })
        .collect();
    let best = diagnosed.iter().copied().max_by_key(|(_, candidate)| {
        diagnosed
            .iter()
            .filter(|(_, other)| {
                other.kind == candidate.kind
                    && other.confidence == candidate.confidence
                    && other.provider == candidate.provider
            })
            .count()
    });

    if let Some((sample, diagnostic)) = best {
        let matching_regions = diagnosed
            .iter()
            .filter(|(_, other)| {
                other.kind == diagnostic.kind
                    && other.confidence == diagnostic.confidence
                    && other.provider == diagnostic.provider
            })
            .count();
        // Same bar the incident itself cleared, so one vendor page cannot
        // label a multi-region outage.
        if matching_regions >= quorum {
            let mut parts = Vec::with_capacity(4);
            if let Some(error) = sample.error.as_deref() {
                parts.push(error.to_owned());
            }
            parts.push(diagnostic.summary());
            // Ahead of the tally: notifications clip this sample, and the fix
            // is worth more to the reader than the vote count.
            parts.push(diagnostic.guidance().to_string());
            if reporting_regions > 1 {
                parts.push(format!(
                    "{matching_regions}/{reporting_regions} reporting regions agree"
                ));
            }
            return Some(parts.join(" · "));
        }
    }

    // Nothing cleared the bar: report the protocol failure rather than guess.
    bad.iter()
        .find_map(|verdict| verdict.bad.iter().find_map(|result| result.error.clone()))
}

/// Anything that is not a clean `Up` is unhealthy: `Down`/`Error` are outages
/// and `Degraded` is a service not meeting its check (slow, partial, rate
/// limited). All three open an incident and none counts as recovery — an
/// incident clears only on a sustained run of genuine `Up`. Exhaustive on
/// purpose: a new `CheckStatus` variant must classify here, never default to
/// healthy and silently auto-close incidents.
fn is_bad(status: CheckStatus) -> bool {
    match status {
        CheckStatus::Down | CheckStatus::Error | CheckStatus::Degraded => true,
        CheckStatus::Up => false,
    }
}

/// Ordering for "worst status at open": hard failures outrank degraded.
fn severity_rank(status: CheckStatus) -> u8 {
    match status {
        CheckStatus::Up => 0,
        CheckStatus::Degraded => 1,
        CheckStatus::Error => 2,
        CheckStatus::Down => 3,
    }
}

fn trailing_bad_run(results: &[CheckResult]) -> &[CheckResult] {
    let split = results
        .iter()
        .rposition(|r| !is_bad(r.status))
        .map(|i| i + 1)
        .unwrap_or(0);
    &results[split..]
}

fn trailing_good_run(results: &[CheckResult]) -> &[CheckResult] {
    let split = results
        .iter()
        .rposition(|r| is_bad(r.status))
        .map(|i| i + 1)
        .unwrap_or(0);
    &results[split..]
}
