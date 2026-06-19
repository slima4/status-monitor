pub mod account_deletion;
pub mod channel_verification;
pub mod incident_alert;
pub mod invitation;
pub mod magic_link;
pub mod subscriber_confirm;
pub mod subscriber_incident;
pub mod subscriber_maintenance;

/// HTML-escape the five entities that matter in element text and double- or
/// single-quoted attribute values. Single owner for every transactional
/// template — the escape set is a security invariant and must not drift
/// between copies.
pub(crate) fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Attribute-context escaping. Same rule as [`html_escape`] today (the quote
/// entities cover `href="…"`); a distinct name keeps call sites self-documenting.
pub(crate) fn attr_escape(input: &str) -> String {
    html_escape(input)
}
