//! Opaque pagination cursor.
//!
//! The wire form is `base64url(offset)` so clients treat it as opaque and the
//! encoding can change without a contract break. Lists hand back a
//! `next_cursor` only when more rows remain; a malformed cursor is a tool
//! invalid-argument error, never a silent reset to page 0.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn encode_offset(offset: usize) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_string())
}

pub fn decode_offset(cursor: &str) -> Option<usize> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    std::str::from_utf8(&bytes).ok()?.parse().ok()
}

/// Offset-paginate a sorted slice: map the `page_size` window at `offset`
/// through `f` and emit a `next_cursor` only when more rows remain. An offset
/// past the end yields an empty page and no cursor (never panics).
pub fn paginate<T, U>(
    items: &[T],
    offset: usize,
    page_size: usize,
    f: impl FnMut(&T) -> U,
) -> (Vec<U>, Option<String>) {
    let total = items.len();
    if offset >= total {
        return (Vec::new(), None);
    }
    let end = offset.saturating_add(page_size).min(total);
    let page = items[offset..end].iter().map(f).collect();
    let next_cursor = (end < total).then(|| encode_offset(end));
    (page, next_cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for n in [0usize, 1, 50, 12_345] {
            assert_eq!(decode_offset(&encode_offset(n)), Some(n));
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(decode_offset("!!!not base64!!!"), None);
        assert_eq!(decode_offset(&URL_SAFE_NO_PAD.encode("not a number")), None);
    }

    #[test]
    fn paginate_windows_and_signals_more() {
        let items: Vec<usize> = (0..10).collect();
        // First page of 4: items 0..4, cursor points at 4.
        let (page, next) = paginate(&items, 0, 4, |n| *n);
        assert_eq!(page, vec![0, 1, 2, 3]);
        assert_eq!(next.and_then(|c| decode_offset(&c)), Some(4));
        // Last partial page: no next cursor.
        let (page, next) = paginate(&items, 8, 4, |n| *n);
        assert_eq!(page, vec![8, 9]);
        assert!(next.is_none());
        // Offset exactly at the end: empty, no cursor.
        let (page, next) = paginate(&items, 10, 4, |n| *n);
        assert!(page.is_empty());
        assert!(next.is_none());
        // Offset past the end: empty, no cursor, no panic.
        let (page, next) = paginate(&items, 50, 4, |n| *n);
        assert!(page.is_empty());
        assert!(next.is_none());
    }
}
