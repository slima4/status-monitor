//! Text shaping shared by every outbound channel: flatten a value that must
//! stay one line, cap one that has a hard length limit. Single owner, because a
//! transport and the template feeding it must not disagree on either rule.

/// Flattens a value destined for a single line — a mail header, a chat title, a
/// paging summary. A control character there would start a new header.
pub(crate) fn single_line(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Char-boundary-safe cap for transports with a hard message-length limit;
/// over-limit text ends in `…` within the cap.
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Like [`truncate_chars`] but for vendors that cap by BYTES (ntfy's 4096);
/// cuts on a char boundary, over-limit text ends in `…` within the cap.
pub(crate) fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let budget = max_bytes.saturating_sub('…'.len_utf8());
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if i + c.len_utf8() > budget {
            break;
        }
        end = i + c.len_utf8();
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::{single_line, truncate_bytes, truncate_chars};

    #[test]
    fn folding_sequences_collapse_into_one_line() {
        assert_eq!(
            single_line("bug\r\nBcc: evil@example.test"),
            "bug Bcc: evil@example.test"
        );
        assert_eq!(single_line("a\tb\u{0}c"), "a b c");
        assert_eq!(single_line("  padded  "), "padded");
    }

    #[test]
    fn truncate_bytes_caps_on_char_boundary() {
        assert_eq!(truncate_bytes("short", 10), "short");
        assert_eq!(truncate_bytes("exact!", 6), "exact!");
        let t = truncate_bytes("overflowing", 8);
        assert!(t.len() <= 8, "{t}");
        assert!(t.ends_with('…'));
        // Multibyte content is capped by BYTES without splitting a char.
        let s = "é".repeat(100); // 200 bytes
        let t = truncate_bytes(&s, 16);
        assert!(t.len() <= 16, "{}", t.len());
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_chars_caps_with_ellipsis() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("exactly", 7), "exactly");
        assert_eq!(truncate_chars("overflowing", 5), "over…");
        // Multi-byte chars count as one; no byte-boundary panic.
        let s = "é".repeat(10);
        assert_eq!(truncate_chars(&s, 4).chars().count(), 4);
        assert!(truncate_chars(&s, 4).ends_with('…'));
    }
}
