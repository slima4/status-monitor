//! Pure status-mapping rules for the public status page.
//!
//! Every classifier derives from *confirmed* incidents — the incident
//! writer's per-region consecutive-failure gate plus region quorum — never
//! from raw check samples. A single failed probe from one region must not
//! repaint a public page; raw samples stay on the operator's region drawer.
//!
//! Independent classifiers:
//!  * [`incident_impact`] — one confirmed incident → its component impact.
//!  * [`component_status`] — worst open impact + active maintenance
//!    → [`PublicComponentStatus`].
//!  * [`overall_state`] — component statuses → [`OverallState`] (the banner).
//!  * [`day_state`] — worst incident impact overlapping one day + whether the
//!    day had checks at all → [`DayState`] cell on the daily history strip.
//!
//! Each is a referentially-transparent function so the truth tables can be
//! exhaustively unit-tested below.

use crate::domain::{DayState, OverallState, OverallStatus, PublicComponentStatus};

/// What one confirmed incident contributes to its component's public state.
/// `Ord` ranks by severity so "worst wins" is `Iterator::max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IncidentImpact {
    Degraded,
    PartialOutage,
    MajorOutage,
}

/// Impact of one confirmed incident. `degraded` is whether the incident
/// opened on a `degraded` check status (slow / rate-limited, not hard-failed);
/// `any_region_up` is whether some region still answered when it opened —
/// present regions in `regions_up` mean a partial, not total, loss.
pub fn incident_impact(degraded: bool, any_region_up: bool) -> IncidentImpact {
    if degraded {
        IncidentImpact::Degraded
    } else if any_region_up {
        IncidentImpact::PartialOutage
    } else {
        IncidentImpact::MajorOutage
    }
}

/// Component status from the worst impact among its open incidents.
/// Maintenance dominates over any failure signal; no open incident →
/// Operational.
pub fn component_status(
    worst: Option<IncidentImpact>,
    maintenance_active: bool,
) -> PublicComponentStatus {
    if maintenance_active {
        return PublicComponentStatus::Maintenance;
    }
    match worst {
        Some(IncidentImpact::MajorOutage) => PublicComponentStatus::MajorOutage,
        Some(IncidentImpact::PartialOutage) => PublicComponentStatus::PartialOutage,
        Some(IncidentImpact::Degraded) => PublicComponentStatus::Degraded,
        None => PublicComponentStatus::Operational,
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

/// Daily history cell: the worst confirmed-incident impact overlapping the
/// day, or Operational. An incident is positive evidence and always paints —
/// even with zero recorded checks (paused mid-incident, manual incident on an
/// unprobed component). `NoData` is reserved for genuinely silent days.
pub fn day_state(has_checks: bool, worst: Option<IncidentImpact>) -> DayState {
    match worst {
        Some(IncidentImpact::MajorOutage) => DayState::MajorOutage,
        Some(IncidentImpact::PartialOutage) => DayState::PartialOutage,
        Some(IncidentImpact::Degraded) => DayState::Degraded,
        None if has_checks => DayState::Operational,
        None => DayState::NoData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── incident_impact truth table ─────────────────────────────────────────

    #[test]
    fn impact_degraded_wins_over_region_split() {
        assert_eq!(incident_impact(true, true), IncidentImpact::Degraded);
        assert_eq!(incident_impact(true, false), IncidentImpact::Degraded);
    }

    #[test]
    fn impact_some_region_up_is_partial() {
        assert_eq!(incident_impact(false, true), IncidentImpact::PartialOutage);
    }

    #[test]
    fn impact_no_region_up_is_major() {
        assert_eq!(incident_impact(false, false), IncidentImpact::MajorOutage);
    }

    #[test]
    fn impact_ord_ranks_major_worst() {
        assert!(IncidentImpact::MajorOutage > IncidentImpact::PartialOutage);
        assert!(IncidentImpact::PartialOutage > IncidentImpact::Degraded);
    }

    // ── component_status truth table ────────────────────────────────────────

    #[test]
    fn component_maintenance_dominates_even_with_major_impact() {
        assert_eq!(
            component_status(Some(IncidentImpact::MajorOutage), true),
            PublicComponentStatus::Maintenance
        );
    }

    #[test]
    fn component_maintenance_dominates_with_no_incident() {
        assert_eq!(
            component_status(None, true),
            PublicComponentStatus::Maintenance
        );
    }

    #[test]
    fn component_no_open_incident_is_operational() {
        assert_eq!(
            component_status(None, false),
            PublicComponentStatus::Operational
        );
    }

    #[test]
    fn component_maps_each_impact() {
        assert_eq!(
            component_status(Some(IncidentImpact::Degraded), false),
            PublicComponentStatus::Degraded
        );
        assert_eq!(
            component_status(Some(IncidentImpact::PartialOutage), false),
            PublicComponentStatus::PartialOutage
        );
        assert_eq!(
            component_status(Some(IncidentImpact::MajorOutage), false),
            PublicComponentStatus::MajorOutage
        );
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
        // Maintenance wins only when *all others* are Operational. Mixed
        // Maintenance + Degraded → MinorDisruption.
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
    fn day_incident_paints_even_without_checks() {
        assert_eq!(
            day_state(false, Some(IncidentImpact::MajorOutage)),
            DayState::MajorOutage
        );
    }

    #[test]
    fn day_silent_without_incident_is_no_data() {
        assert_eq!(day_state(false, None), DayState::NoData);
    }

    #[test]
    fn day_with_checks_and_no_incident_is_operational() {
        assert_eq!(day_state(true, None), DayState::Operational);
    }

    #[test]
    fn day_maps_each_impact() {
        assert_eq!(
            day_state(true, Some(IncidentImpact::Degraded)),
            DayState::Degraded
        );
        assert_eq!(
            day_state(true, Some(IncidentImpact::PartialOutage)),
            DayState::PartialOutage
        );
        assert_eq!(
            day_state(true, Some(IncidentImpact::MajorOutage)),
            DayState::MajorOutage
        );
    }
}
