//! Aggregated askama filter functions.
//!
//! askama codegens `filters::<name>(...)` at every derive site, so every
//! template-deriving module imports this module as `filters` via
//! `use crate::web::filters;`. Group filters by concern below: a new
//! reusable filter lands in the most-specific sub-module (or a new one)
//! and is re-exported here so the derive sites don't have to know.

pub use self::display::*;
pub use self::static_refs::*;

/// Cache-busting asset URLs + AGPL-3.0 source-offer metadata.
mod static_refs {
    /// `{{ "css/app.css"|asset }}` → the cache-busting URL for that asset.
    #[askama::filter_fn]
    pub fn asset(value: &str, _: &dyn askama::Values) -> askama::Result<String> {
        Ok(crate::web::assets::url(value))
    }

    /// AGPL-3.0 §13 source offer. `build.rs` bakes the repository URL and
    /// build commit in via `rustc-env`. `source_url` deep-links to the
    /// exact source the running binary was built from — `…/tree/<commit>`
    /// when the commit is known, the repo root otherwise. The piped value
    /// is unused — invoked as `{{ ""|source_url }}`.
    static SOURCE_URL: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| match env!("SM_SOURCE_COMMIT") {
            "" => env!("SM_SOURCE_URL").to_string(),
            commit => format!("{}/tree/{commit}", env!("SM_SOURCE_URL")),
        });

    #[askama::filter_fn]
    pub fn source_url(_: &str, _: &dyn askama::Values) -> askama::Result<&'static str> {
        Ok(SOURCE_URL.as_str())
    }

    #[askama::filter_fn]
    pub fn source_commit(_: &str, _: &dyn askama::Values) -> askama::Result<&'static str> {
        Ok(env!("SM_SOURCE_COMMIT"))
    }

    /// Crate version baked at compile time. Public build string, safe to
    /// show pre-auth — no host/tenant data. Invoked as `{{ ""|version }}`.
    #[askama::filter_fn]
    pub fn version(_: &str, _: &dyn askama::Values) -> askama::Result<&'static str> {
        Ok(env!("CARGO_PKG_VERSION"))
    }

    /// Evaluated once per process; a prod process crossing Jan 1 picks up
    /// the new year on the next deploy.
    #[askama::filter_fn]
    pub fn current_year(_: &str, _: &dyn askama::Values) -> askama::Result<i32> {
        use chrono::Datelike;
        static YEAR: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
        Ok(*YEAR.get_or_init(|| chrono::Utc::now().year()))
    }
}

/// Format raw values (timestamps, durations) by writing straight into the
/// template output buffer. The per-row hot path on the incidents table
/// uses these so a 100-row render makes zero intermediate `String`
/// allocations for time/duration columns.
mod display {
    use chrono::DateTime;
    use chrono::Utc;
    use chrono::format::{DelayedFormat, StrftimeItems};

    /// `{{ ts|iso_ts }}` → `"2026-05-13T12:00:00Z"`.
    #[askama::filter_fn]
    pub fn iso_ts(
        value: &DateTime<Utc>,
        _: &dyn askama::Values,
    ) -> askama::Result<DelayedFormat<StrftimeItems<'static>>> {
        Ok(value.format("%Y-%m-%dT%H:%M:%SZ"))
    }

    /// `{{ ts|human_ts }}` → `"2026-05-13 12:00 UTC"`.
    #[askama::filter_fn]
    pub fn human_ts(
        value: &DateTime<Utc>,
        _: &dyn askama::Values,
    ) -> askama::Result<DelayedFormat<StrftimeItems<'static>>> {
        Ok(value.format("%Y-%m-%d %H:%M UTC"))
    }

    /// `{{ secs|humanize_dur }}` → `"7m"` / `"2h 14m"` / `"1d 1h"`.
    #[askama::filter_fn]
    pub fn humanize_dur(
        value: &i64,
        _: &dyn askama::Values,
    ) -> askama::Result<crate::web::views::HumanDur> {
        Ok(crate::web::views::HumanDur(*value))
    }
}
