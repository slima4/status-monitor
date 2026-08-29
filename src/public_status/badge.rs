//! SVG status badge rendering.
//!
//! Produces a shields.io-style two-segment badge that operators can embed in
//! README files or external dashboards.

use badgelib::{Badge, Color, Style};

use crate::domain::{OverallState, PublicComponentStatus};

/// Color palette aligned with the public status page UI.
const COLOR_GREEN: &str = "#4c1";
const COLOR_YELLOW: &str = "#dfb317";
const COLOR_ORANGE: &str = "#fe7d37";
const COLOR_RED: &str = "#e05d44";
const COLOR_BLUE: &str = "#007ec6";
const COLOR_GREY: &str = "#9f9f9f";

/// Mapping from overall page state to (status text, color).
pub fn overall_badge(state: OverallState) -> (&'static str, &'static str) {
    match state {
        OverallState::Operational => ("operational", COLOR_GREEN),
        OverallState::MinorDisruption => ("minor disruption", COLOR_YELLOW),
        OverallState::Maintenance => ("maintenance", COLOR_BLUE),
        OverallState::PartialOutage => ("partial outage", COLOR_ORANGE),
        OverallState::MajorOutage => ("major outage", COLOR_RED),
    }
}

/// Mapping from per-component status to (status text, color).
pub fn component_badge(state: PublicComponentStatus) -> (&'static str, &'static str) {
    match state {
        PublicComponentStatus::Operational => ("operational", COLOR_GREEN),
        PublicComponentStatus::Degraded => ("degraded", COLOR_YELLOW),
        PublicComponentStatus::Maintenance => ("maintenance", COLOR_BLUE),
        PublicComponentStatus::PartialOutage => ("partial outage", COLOR_ORANGE),
        PublicComponentStatus::MajorOutage => ("major outage", COLOR_RED),
        PublicComponentStatus::NoData => ("no data", COLOR_GREY),
    }
}

/// Build a shields.io-style SVG badge. The label segment is gray; the status
/// segment carries the state color.
pub fn render_badge(label: &str, status: &str, color: &str, style: Style) -> String {
    Badge::new()
        .label(label)
        .label_color(Color::Hex("555".into()))
        .value(status)
        .value_color(Color::try_from(color).expect("static badge color is valid"))
        .style(style)
        .to_svg()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overall_state_covers_every_variant() {
        for s in [
            OverallState::Operational,
            OverallState::MinorDisruption,
            OverallState::Maintenance,
            OverallState::PartialOutage,
            OverallState::MajorOutage,
        ] {
            let (text, color) = overall_badge(s);
            assert!(!text.is_empty());
            assert!(color.starts_with('#'));
        }
    }

    #[test]
    fn component_state_covers_every_variant() {
        for s in [
            PublicComponentStatus::Operational,
            PublicComponentStatus::Degraded,
            PublicComponentStatus::Maintenance,
            PublicComponentStatus::PartialOutage,
            PublicComponentStatus::MajorOutage,
            PublicComponentStatus::NoData,
        ] {
            let (text, color) = component_badge(s);
            assert!(!text.is_empty());
            assert!(color.starts_with('#'));
        }
    }

    #[test]
    fn a_silent_component_badge_is_grey_and_says_no_data() {
        // Never a green badge: a README embedding this must not read as a
        // health claim for something that has reported nothing.
        let (text, color) = component_badge(PublicComponentStatus::NoData);
        assert_eq!(text, "no data");
        assert_ne!(color, COLOR_GREEN);
    }

    #[test]
    fn render_produces_valid_xml_root() {
        let svg = render_badge("API", "operational", COLOR_GREEN, Style::Flat);
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn render_escapes_xml_special_chars() {
        let svg = render_badge("a&b<c>", "x\"y'z", COLOR_GREEN, Style::Flat);
        assert!(svg.contains("a&amp;b&lt;c&gt;"));
        assert!(svg.contains("x&quot;y'z"));
        assert!(!svg.contains("a&b<c>"));
        assert!(!svg.contains("x\"y'z"));
    }

    #[test]
    fn render_includes_accessible_title() {
        let svg = render_badge("API", "degraded", COLOR_YELLOW, Style::Flat);
        assert!(svg.contains("aria-label=\"API: degraded\""));
        assert!(svg.contains("<title>API: degraded</title>"));
    }

    #[test]
    fn render_carries_state_color() {
        let svg = render_badge("X", "major outage", COLOR_RED, Style::Flat);
        assert!(svg.contains(COLOR_RED));
    }

    #[test]
    fn render_supports_every_exposed_style() {
        for style in [Style::Flat, Style::FlatSquare, Style::ForTheBadge] {
            let svg = render_badge("API", "operational", COLOR_GREEN, style);
            assert!(svg.starts_with("<svg "));
            assert!(svg.trim_end().ends_with("</svg>"));
        }
    }
}
