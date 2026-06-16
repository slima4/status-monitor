//! Shared region display labels for the assignment + filter dropdowns, so a raw
//! region id never reaches a user-facing control.

use std::collections::HashMap;

use crate::storage::RegionOption;

pub struct LabeledRegion {
    pub id: String,
    pub label: String,
}

/// Human label for a region: its display name ([`RegionOption::display_name`])
/// with the city appended in parentheses when set.
pub fn region_label(region: &RegionOption) -> String {
    let base = region.display_name();
    let city = region.city.trim();
    let text = if city.is_empty() {
        base.to_string()
    } else {
        format!("{base} ({city})")
    };
    match region.flag() {
        Some(flag) => format!("{flag} {text}"),
        None => text,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn region(name: &str, city: &str, country: Option<&str>) -> RegionOption {
        RegionOption {
            id: "r".into(),
            name: name.into(),
            city: city.into(),
            country_code: country.map(str::to_string),
            continent: None,
            latitude: None,
            longitude: None,
        }
    }

    #[test]
    fn label_prepends_flag_and_appends_city() {
        assert_eq!(
            region_label(&region("EU", "Helsinki", Some("FI"))),
            "🇫🇮 EU (Helsinki)"
        );
        assert_eq!(
            region_label(&region("EU", "Helsinki", None)),
            "EU (Helsinki)"
        );
        assert_eq!(region_label(&region("EU", "", Some("FI"))), "🇫🇮 EU");
        assert_eq!(region_label(&region("EU", "", None)), "EU");
    }
}
