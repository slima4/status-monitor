//! Shared XML escape helper for the public-status surface.
//!
//! Both the RSS feed (`source.rs::build_rss`) and the SVG badge
//! (`badge.rs::render_badge`) embed operator-supplied strings into XML
//! documents. Escapes the five standard XML entities; returns `Cow::Borrowed`
//! when the input is already safe so the hot path avoids allocation.

use std::borrow::Cow;

pub fn xml_escape(s: &str) -> Cow<'_, str> {
    if !s.contains(['&', '<', '>', '"', '\'']) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrows_when_no_escapes_needed() {
        let s = "operational";
        let out = xml_escape(s);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(&*out, "operational");
    }

    #[test]
    fn escapes_all_five_entities() {
        let out = xml_escape("a<b&c>d\"e'f");
        assert_eq!(&*out, "a&lt;b&amp;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn preserves_unicode() {
        let out = xml_escape("café & co");
        assert_eq!(&*out, "café &amp; co");
    }
}
