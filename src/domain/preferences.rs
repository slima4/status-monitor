//! Boundary validators for `users.locale` and `users.timezone`. Any future
//! setter must call these; the email renderer trusts the column.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::domain::AppTheme;

/// The per-user display preferences read together on every login and on the
/// account page — grouped so they ride one query and one cookie-issue pass
/// instead of a getter (and a forgotten cookie) per preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayPrefs {
    pub theme: AppTheme,
    pub time_format: TimeFormat,
}

/// Per-user 12h/24h preference for client-side timestamp rendering. `Auto`
/// keeps the browser-locale default; the other two force the hour cycle. The
/// string forms are the persisted (`users.time_format`) and JSON wire values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum TimeFormat {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "12h")]
    TwelveHour,
    #[serde(rename = "24h")]
    TwentyFourHour,
}

impl TimeFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::TwelveHour => "12h",
            Self::TwentyFourHour => "24h",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "12h" => Self::TwelveHour,
            "24h" => Self::TwentyFourHour,
            "auto" => Self::Auto,
            other => {
                tracing::warn!(value = other, "unknown user.time_format in DB, using auto");
                Self::Auto
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefError {
    LocaleFormat,
    TimezoneUnknown,
}

impl std::fmt::Display for PrefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::LocaleFormat => "locale must be a BCP-47 tag like 'en' or 'en-US'",
            Self::TimezoneUnknown => "timezone must be a known IANA name like 'Europe/Nicosia'",
        };
        f.write_str(s)
    }
}

impl std::error::Error for PrefError {}

pub fn validate_locale(s: &str) -> Result<(), PrefError> {
    let len = s.len();
    if !(2..=35).contains(&len) {
        return Err(PrefError::LocaleFormat);
    }
    let mut parts = s.split('-');
    let lang = parts.next().ok_or(PrefError::LocaleFormat)?;
    if !(2..=3).contains(&lang.len()) || !lang.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err(PrefError::LocaleFormat);
    }
    for sub in parts {
        let len = sub.len();
        let ok = match len {
            2 => sub.bytes().all(|b| b.is_ascii_uppercase()),
            3 => sub.bytes().all(|b| b.is_ascii_digit()),
            4 => {
                let bytes = sub.as_bytes();
                bytes[0].is_ascii_uppercase() && bytes[1..].iter().all(|b| b.is_ascii_lowercase())
            }
            5..=8 => sub.bytes().all(|b| b.is_ascii_alphanumeric()),
            _ => false,
        };
        if !ok {
            return Err(PrefError::LocaleFormat);
        }
    }
    Ok(())
}

pub fn validate_timezone(s: &str) -> Result<(), PrefError> {
    chrono_tz::Tz::from_str(s)
        .map(|_| ())
        .map_err(|_| PrefError::TimezoneUnknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format_round_trips_through_db_and_json() {
        for f in [
            TimeFormat::Auto,
            TimeFormat::TwelveHour,
            TimeFormat::TwentyFourHour,
        ] {
            assert_eq!(TimeFormat::from_db(f.as_str()), f);
            let json = serde_json::to_string(&f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.as_str()));
            assert_eq!(serde_json::from_str::<TimeFormat>(&json).unwrap(), f);
        }
        // Unknown stored value degrades to the safe default.
        assert_eq!(TimeFormat::from_db("garbage"), TimeFormat::Auto);
        // Unknown JSON value is rejected, not silently defaulted.
        assert!(serde_json::from_str::<TimeFormat>("\"36h\"").is_err());
    }

    #[test]
    fn locale_accepts_basic_shapes() {
        for ok in [
            "en",
            "fr",
            "de",
            "en-US",
            "pt-BR",
            "zh-Hans",
            "zh-Hans-CN",
            "es-419",
        ] {
            assert!(validate_locale(ok).is_ok(), "{ok} should pass");
        }
    }

    #[test]
    fn locale_rejects_garbage() {
        for bad in [
            "",
            "x",
            "EN",
            "en_US",
            "en-us",
            "english",
            "en-USA",
            "1234",
            "en-US-extra-extra-extra-extra-extra-extra-extra-extra",
        ] {
            assert!(validate_locale(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn timezone_accepts_canonical_iana_names() {
        for ok in [
            "UTC",
            "Europe/Nicosia",
            "America/Argentina/Ushuaia",
            "Asia/Tokyo",
        ] {
            assert!(validate_timezone(ok).is_ok(), "{ok} should pass");
        }
    }

    #[test]
    fn timezone_rejects_unknown() {
        for bad in [
            "",
            "Mars/Olympus_Mons",
            "europe/nicosia",
            "Europe-Nicosia",
            "PST",
        ] {
            assert!(
                validate_timezone(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
