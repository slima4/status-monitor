//! Shared region display labels for the assignment + filter dropdowns, so a raw
//! region id never reaches a user-facing control.

use std::collections::HashMap;

use crate::storage::RegionOption;

pub struct LabeledRegion {
    pub id: String,
    pub label: String,
}

/// Human label for a region: its display name ([`RegionOption::display_name`])
/// with the location appended in parentheses when set.
pub fn region_label(region: &RegionOption) -> String {
    let base = region.display_name();
    let location = region.location.trim();
    if location.is_empty() {
        base.to_string()
    } else {
        format!("{base} ({location})")
    }
}

/// Map region ids (an org's or target's assigned set) to display labels from the
/// catalog, preserving input order. An id absent from the catalog falls back to
/// itself as the label.
pub fn labeled_regions(catalog: &[RegionOption], ids: Vec<String>) -> Vec<LabeledRegion> {
    let by_id: HashMap<&str, &RegionOption> = catalog.iter().map(|r| (r.id.as_str(), r)).collect();
    ids.into_iter()
        .map(|id| {
            let label = by_id
                .get(id.as_str())
                .map(|r| region_label(r))
                .unwrap_or_else(|| id.clone());
            LabeledRegion { id, label }
        })
        .collect()
}
