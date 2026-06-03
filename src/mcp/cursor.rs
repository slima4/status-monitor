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
}
