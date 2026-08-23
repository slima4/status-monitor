//! Email shape normalization. Single owner of the "is this string a plausible
//! email" predicate so the invitation and magic-link surfaces stay aligned.
//!
//! Strict parsing belongs to the SMTP layer; this only filters obvious junk
//! (empty, no `@`, too long). Anything that survives the gate either matches a
//! real user (verify endpoints) or is dropped silently by the anti-enumeration
//! handlers (request endpoints).

pub const MAX_EMAIL_LEN: usize = 254;

/// Trim, length-cap, and require an `@`. `None` for malformed input — callers
/// that want a structured 400 wrap this with their own `bad_request_field`.
///
/// Also refuses control characters and inner whitespace. `trim` only reaches
/// the ends, so `a@b\r\nBcc: …` would otherwise survive the gate and reach a
/// mail transport intact.
pub fn normalize(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let local_at = trimmed.find('@')?;
    if local_at == 0 || trimmed.len() <= local_at + 1 || trimmed.len() > MAX_EMAIL_LEN {
        return None;
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    Some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_whitespace_and_no_at() {
        assert!(normalize("").is_none());
        assert!(normalize("   ").is_none());
        assert!(normalize("not-an-email").is_none());
    }

    #[test]
    fn rejects_at_with_no_local_or_domain() {
        assert!(normalize("@").is_none());
        assert!(normalize("@example.test").is_none());
        assert!(normalize("alice@").is_none());
    }

    #[test]
    fn trims_and_accepts_well_formed() {
        assert_eq!(
            normalize("  alice@example.com  "),
            Some("alice@example.com")
        );
    }

    #[test]
    fn rejects_anything_that_could_carry_a_second_header() {
        // `trim` reaches the ends only, so an address is the one attacker-held
        // string that could reach a mail transport with a newline in it.
        assert!(normalize("a@b.test\r\nBcc: evil@example.test").is_none());
        assert!(normalize("a@b.test\nBcc: evil@example.test").is_none());
        assert!(normalize("a@b\u{0}.test").is_none());
        assert!(normalize("a b@example.test").is_none());
        // And the ordinary case still passes.
        assert_eq!(normalize(" a@b.test "), Some("a@b.test"));
    }

    #[test]
    fn rejects_oversized() {
        let big = format!("{}@x", "a".repeat(MAX_EMAIL_LEN));
        assert!(normalize(&big).is_none());
    }
}
