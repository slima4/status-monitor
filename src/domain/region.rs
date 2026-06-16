//! Region geography: the continent a region sits in, used to group the
//! monitor region picker. Stored as a stable lowercase slug (guarded by a DB
//! CHECK), rendered with a human label.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Continent {
    Africa,
    Asia,
    Europe,
    NorthAmerica,
    SouthAmerica,
    Oceania,
    Antarctica,
}

impl Continent {
    /// Picker group order.
    pub const ALL: [Continent; 7] = [
        Continent::Africa,
        Continent::Asia,
        Continent::Europe,
        Continent::NorthAmerica,
        Continent::SouthAmerica,
        Continent::Oceania,
        Continent::Antarctica,
    ];

    /// Stable storage slug — the exact set the DB CHECK constraint allows.
    pub fn slug(self) -> &'static str {
        match self {
            Continent::Africa => "africa",
            Continent::Asia => "asia",
            Continent::Europe => "europe",
            Continent::NorthAmerica => "north_america",
            Continent::SouthAmerica => "south_america",
            Continent::Oceania => "oceania",
            Continent::Antarctica => "antarctica",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Continent::Africa => "Africa",
            Continent::Asia => "Asia",
            Continent::Europe => "Europe",
            Continent::NorthAmerica => "North America",
            Continent::SouthAmerica => "South America",
            Continent::Oceania => "Oceania",
            Continent::Antarctica => "Antarctica",
        }
    }

    pub fn parse(slug: &str) -> Option<Continent> {
        Continent::ALL.into_iter().find(|c| c.slug() == slug)
    }

    /// Comma-joined slugs for error messages.
    pub fn slug_list() -> String {
        Continent::ALL
            .iter()
            .map(|c| c.slug())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Normalize an ISO 3166-1 alpha-2 country code: trim, require exactly two
/// ASCII letters, uppercase. `None` when absent or malformed. Single source of
/// the country-code rule shared by validation and flag rendering.
pub fn normalize_country_code(code: &str) -> Option<String> {
    let code = code.trim();
    (code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

/// Unicode flag emoji for an alpha-2 country code (e.g. `FI` → 🇫🇮), built from
/// the two regional-indicator codepoints. `None` unless the code is valid.
pub fn country_flag(code: &str) -> Option<String> {
    normalize_country_code(code)?
        .chars()
        .map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_label_roundtrip() {
        for c in Continent::ALL {
            assert_eq!(Continent::parse(c.slug()), Some(c));
            assert!(!c.label().is_empty());
        }
        assert_eq!(Continent::parse("atlantis"), None);
        assert_eq!(Continent::NorthAmerica.slug(), "north_america");
        assert_eq!(Continent::NorthAmerica.label(), "North America");
    }

    #[test]
    fn country_code_normalizes_and_flags() {
        assert_eq!(normalize_country_code("fi").as_deref(), Some("FI"));
        assert_eq!(normalize_country_code(" us ").as_deref(), Some("US"));
        assert_eq!(normalize_country_code("USA"), None);
        assert_eq!(normalize_country_code("F"), None);
        assert_eq!(normalize_country_code("A1"), None);
        assert_eq!(normalize_country_code(""), None);

        assert_eq!(country_flag("FI").as_deref(), Some("🇫🇮"));
        assert_eq!(country_flag("us").as_deref(), Some("🇺🇸"));
        assert_eq!(country_flag("USA"), None);
        assert_eq!(country_flag("12"), None);
    }
}
