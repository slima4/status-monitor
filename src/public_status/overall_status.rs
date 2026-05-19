//! Pure status-mapping rules for the public status page.
//!
//! Three independent classifiers:
//!  * [`component_status`] — last-5-minutes result counts + active maintenance
//!    → [`PublicComponentStatus`].
//!  * [`overall_state`] — component statuses → [`OverallState`] (the banner).
//!  * [`day_state`] — per-minute counts for one day + maintenance overlap →
//!    [`DayState`] cell on the daily history strip.
//!
//! Each is a referentially-transparent function so the truth tables can be
//! exhaustively unit-tested below.

use crate::domain::{
    CheckResult, CheckStatus, DayState, OverallState, OverallStatus, PublicComponentStatus,
};

/// Counts of check statuses over some bucket (a window of recent checks, or a
/// single minute of the daily history strip).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub up: u32,
    pub down: u32,
    pub degraded: u32,
    pub error: u32,
}

impl Counters {
    pub fn total(&self) -> u32 {
        self.up + self.down + self.degraded + self.error
    }

    /// `down` + `error` — the hard-failure count used by the mapping rules.
    pub fn hard_failed(&self) -> u32 {
        self.down + self.error
    }

    pub fn has_degraded(&self) -> bool {
        self.degraded > 0
    }

    pub fn has_hard_failure(&self) -> bool {
        self.hard_failed() > 0
    }

    /// Treats `degraded` as success for the purposes of the ≥50% rule —
    /// matches the wording "≥ 50% of checks **down or error**".
    pub fn hard_failed_ratio_ge_half(&self) -> bool {
        let total = self.total();
        total > 0 && self.hard_failed() * 2 >= total
    }

    pub fn from_results(results: &[CheckResult]) -> Self {
        let mut c = Self::default();
        for r in results {
            match r.status {
                CheckStatus::Up => c.up += 1,
                CheckStatus::Down => c.down += 1,
                CheckStatus::Degraded => c.degraded += 1,
                CheckStatus::Error => c.error += 1,
            }
        }
        c
    }
}

/// Component status. Maintenance dominates over any failure signal.
pub fn component_status(c: &Counters, maintenance_active: bool) -> PublicComponentStatus {
    if maintenance_active {
        return PublicComponentStatus::Maintenance;
    }
    if c.total() == 0 {
        // No recent data — render as Operational rather than fabricating an outage.
        return PublicComponentStatus::Operational;
    }
    if !c.has_hard_failure() {
        return if c.has_degraded() {
            PublicComponentStatus::Degraded
        } else {
            PublicComponentStatus::Operational
        };
    }
    if c.hard_failed_ratio_ge_half() {
        PublicComponentStatus::MajorOutage
    } else {
        PublicComponentStatus::PartialOutage
    }
}

/// Overall page banner state from per-component statuses.
///
/// Empty component list → `Operational`.
pub fn overall_state(components: &[PublicComponentStatus]) -> OverallState {
    if components.contains(&PublicComponentStatus::MajorOutage) {
        return OverallState::MajorOutage;
    }
    if components.contains(&PublicComponentStatus::PartialOutage) {
        return OverallState::PartialOutage;
    }
    if components.contains(&PublicComponentStatus::Degraded) {
        return OverallState::MinorDisruption;
    }
    let any_maintenance = components.contains(&PublicComponentStatus::Maintenance);
    let others_operational = components.iter().all(|s| {
        matches!(
            s,
            PublicComponentStatus::Maintenance | PublicComponentStatus::Operational
        )
    });
    if any_maintenance && others_operational {
        return OverallState::Maintenance;
    }
    OverallState::Operational
}

pub fn overall_label(state: OverallState) -> &'static str {
    match state {
        OverallState::Operational => "All Systems Operational",
        OverallState::Maintenance => "Maintenance in progress",
        OverallState::MinorDisruption => "Minor Service Disruption",
        OverallState::PartialOutage => "Partial System Outage",
        OverallState::MajorOutage => "Major System Outage",
    }
}

pub fn overall_status(state: OverallState) -> OverallStatus {
    OverallStatus {
        state,
        label: overall_label(state).to_string(),
    }
}

/// Daily history cell from per-minute counters + whether *any* minute of
/// the day overlapped a maintenance window.
///
/// `minutes` is the slice of minute-level counters for the day; an empty
/// slice means there were no recorded checks at all that day → `NoData`.
/// Maintenance dominates over outages.
pub fn day_state(maintenance_covers_any_minute: bool, minutes: &[Counters]) -> DayState {
    if minutes.is_empty() {
        return DayState::NoData;
    }
    if maintenance_covers_any_minute {
        return DayState::Maintenance;
    }
    let mut any_major = false;
    let mut any_partial = false;
    let mut any_degraded_only = false;
    for m in minutes {
        if m.total() == 0 {
            continue;
        }
        if m.hard_failed_ratio_ge_half() {
            any_major = true;
        } else if m.has_hard_failure() {
            any_partial = true;
        } else if m.has_degraded() {
            any_degraded_only = true;
        }
    }
    if any_major {
        DayState::MajorOutage
    } else if any_partial {
        DayState::PartialOutage
    } else if any_degraded_only {
        DayState::Degraded
    } else {
        DayState::Operational
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(up: u32, down: u32, degraded: u32, error: u32) -> Counters {
        Counters {
            up,
            down,
            degraded,
            error,
        }
    }

    // ── component_status truth table ────────────────────────────────────────

    #[test]
    fn component_maintenance_dominates_even_with_all_up() {
        assert_eq!(
            component_status(&c(5, 0, 0, 0), true),
            PublicComponentStatus::Maintenance
        );
    }

    #[test]
    fn component_maintenance_dominates_even_with_major_outage() {
        assert_eq!(
            component_status(&c(0, 5, 0, 0), true),
            PublicComponentStatus::Maintenance
        );
    }

    #[test]
    fn component_all_up_is_operational() {
        assert_eq!(
            component_status(&c(5, 0, 0, 0), false),
            PublicComponentStatus::Operational
        );
    }

    #[test]
    fn component_no_data_renders_operational() {
        assert_eq!(
            component_status(&c(0, 0, 0, 0), false),
            PublicComponentStatus::Operational
        );
    }

    #[test]
    fn component_one_degraded_no_hard_failure_is_degraded() {
        assert_eq!(
            component_status(&c(4, 0, 1, 0), false),
            PublicComponentStatus::Degraded
        );
    }

    #[test]
    fn component_all_degraded_no_hard_failure_is_degraded() {
        assert_eq!(
            component_status(&c(0, 0, 5, 0), false),
            PublicComponentStatus::Degraded
        );
    }

    #[test]
    fn component_under_half_down_is_partial_outage() {
        // 1 of 5 down → 20% → PartialOutage
        assert_eq!(
            component_status(&c(4, 1, 0, 0), false),
            PublicComponentStatus::PartialOutage
        );
    }

    #[test]
    fn component_under_half_error_is_partial_outage() {
        assert_eq!(
            component_status(&c(4, 0, 0, 1), false),
            PublicComponentStatus::PartialOutage
        );
    }

    #[test]
    fn component_mixed_under_half_failure_with_degraded_is_partial() {
        // Degraded does NOT promote past PartialOutage when any hard failure exists.
        assert_eq!(
            component_status(&c(3, 1, 1, 0), false),
            PublicComponentStatus::PartialOutage
        );
    }

    #[test]
    fn component_exactly_half_down_is_major_outage() {
        // 2 of 4 down → exactly 50% → ≥50% → MajorOutage.
        assert_eq!(
            component_status(&c(2, 2, 0, 0), false),
            PublicComponentStatus::MajorOutage
        );
    }

    #[test]
    fn component_majority_error_is_major_outage() {
        assert_eq!(
            component_status(&c(1, 0, 0, 4), false),
            PublicComponentStatus::MajorOutage
        );
    }

    #[test]
    fn component_all_down_is_major_outage() {
        assert_eq!(
            component_status(&c(0, 5, 0, 0), false),
            PublicComponentStatus::MajorOutage
        );
    }

    #[test]
    fn component_from_results_helper_classifies_correctly() {
        use chrono::Utc;
        use uuid::Uuid;
        let r = |s: CheckStatus| CheckResult {
            target_id: Uuid::nil(),
            org_id: Uuid::nil(),
            timestamp: Utc::now(),
            status: s,
            duration_ms: 1,
            dns_ms: None,
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: None,
        };
        let results = vec![
            r(CheckStatus::Up),
            r(CheckStatus::Up),
            r(CheckStatus::Degraded),
        ];
        let c = Counters::from_results(&results);
        assert_eq!(component_status(&c, false), PublicComponentStatus::Degraded);
    }

    // ── overall_state truth table ───────────────────────────────────────────

    #[test]
    fn overall_empty_is_operational() {
        assert_eq!(overall_state(&[]), OverallState::Operational);
    }

    #[test]
    fn overall_all_operational_is_operational() {
        let s = [PublicComponentStatus::Operational; 3];
        assert_eq!(overall_state(&s), OverallState::Operational);
    }

    #[test]
    fn overall_maintenance_with_operational_is_maintenance() {
        let s = [
            PublicComponentStatus::Maintenance,
            PublicComponentStatus::Operational,
        ];
        assert_eq!(overall_state(&s), OverallState::Maintenance);
    }

    #[test]
    fn overall_only_maintenance_is_maintenance() {
        let s = [PublicComponentStatus::Maintenance];
        assert_eq!(overall_state(&s), OverallState::Maintenance);
    }

    #[test]
    fn overall_maintenance_plus_degraded_is_minor_disruption() {
        // Spec: "any Degraded, none worse → MinorDisruption" — Maintenance is
        // NOT "worse", but Maintenance only wins when *all others* are
        // Operational. Mixed Maintenance + Degraded → MinorDisruption.
        let s = [
            PublicComponentStatus::Maintenance,
            PublicComponentStatus::Degraded,
        ];
        assert_eq!(overall_state(&s), OverallState::MinorDisruption);
    }

    #[test]
    fn overall_any_degraded_is_minor_disruption() {
        let s = [
            PublicComponentStatus::Operational,
            PublicComponentStatus::Degraded,
        ];
        assert_eq!(overall_state(&s), OverallState::MinorDisruption);
    }

    #[test]
    fn overall_partial_with_degraded_is_partial() {
        let s = [
            PublicComponentStatus::Degraded,
            PublicComponentStatus::PartialOutage,
        ];
        assert_eq!(overall_state(&s), OverallState::PartialOutage);
    }

    #[test]
    fn overall_partial_alone_is_partial() {
        let s = [PublicComponentStatus::PartialOutage];
        assert_eq!(overall_state(&s), OverallState::PartialOutage);
    }

    #[test]
    fn overall_major_dominates_partial() {
        let s = [
            PublicComponentStatus::PartialOutage,
            PublicComponentStatus::MajorOutage,
        ];
        assert_eq!(overall_state(&s), OverallState::MajorOutage);
    }

    #[test]
    fn overall_major_alone_is_major() {
        let s = [PublicComponentStatus::MajorOutage];
        assert_eq!(overall_state(&s), OverallState::MajorOutage);
    }

    #[test]
    fn overall_labels_match_expected_strings() {
        assert_eq!(
            overall_label(OverallState::Operational),
            "All Systems Operational"
        );
        assert_eq!(
            overall_label(OverallState::Maintenance),
            "Maintenance in progress"
        );
        assert_eq!(
            overall_label(OverallState::MinorDisruption),
            "Minor Service Disruption"
        );
        assert_eq!(
            overall_label(OverallState::PartialOutage),
            "Partial System Outage"
        );
        assert_eq!(
            overall_label(OverallState::MajorOutage),
            "Major System Outage"
        );
    }

    // ── day_state truth table ───────────────────────────────────────────────

    #[test]
    fn day_no_rows_is_no_data() {
        assert_eq!(day_state(false, &[]), DayState::NoData);
    }

    #[test]
    fn day_maintenance_dominates_even_with_major_outage_minutes() {
        let mins = [c(0, 10, 0, 0)];
        assert_eq!(day_state(true, &mins), DayState::Maintenance);
    }

    #[test]
    fn day_any_major_minute_wins() {
        let mins = [c(10, 0, 0, 0), c(2, 8, 0, 0), c(10, 0, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::MajorOutage);
    }

    #[test]
    fn day_exact_half_failure_minute_is_major() {
        // ≥50% rule: 5/10 → Major
        let mins = [c(5, 5, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::MajorOutage);
    }

    #[test]
    fn day_under_half_failure_minute_is_partial() {
        let mins = [c(9, 1, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::PartialOutage);
    }

    #[test]
    fn day_partial_minute_overrides_degraded_only_minute() {
        let mins = [c(0, 0, 5, 0), c(9, 1, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::PartialOutage);
    }

    #[test]
    fn day_only_degraded_minute_is_degraded() {
        let mins = [c(0, 0, 5, 0)];
        assert_eq!(day_state(false, &mins), DayState::Degraded);
    }

    #[test]
    fn day_mixed_up_and_degraded_minutes_is_degraded() {
        let mins = [c(10, 0, 0, 0), c(5, 0, 2, 0), c(10, 0, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::Degraded);
    }

    #[test]
    fn day_all_up_is_operational() {
        let mins = [c(10, 0, 0, 0), c(10, 0, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::Operational);
    }

    #[test]
    fn day_skips_empty_minutes_does_not_make_no_data() {
        // A handful of minutes with data, plus some empty-total minute rows
        // (artifact of an aggregate query) → still classified, not NoData.
        let mins = [c(10, 0, 0, 0), Counters::default(), c(10, 0, 0, 0)];
        assert_eq!(day_state(false, &mins), DayState::Operational);
    }
}
