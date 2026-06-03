//! PKCE (RFC 7636) — S256 only.
//!
//! `plain` is intentionally unsupported (OAuth 2.1 forbids it for new servers).
//! Verification is constant-time on the challenge compare.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// RFC 7636 §4.1 — `code_verifier` = 43–128 chars from the unreserved set
/// `[A-Za-z0-9-._~]`.
pub fn is_valid_verifier(verifier: &str) -> bool {
    let len = verifier.len();
    (43..=128).contains(&len)
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// A `code_challenge` is the base64url(no-pad) of a 32-byte SHA-256 → 43 chars,
/// same unreserved alphabet.
pub fn is_valid_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// True iff `BASE64URL(SHA256(verifier)) == challenge` (constant-time). Rejects
/// a malformed verifier up front.
pub fn verify_s256(verifier: &str, challenge: &str) -> bool {
    if !is_valid_verifier(verifier) {
        return false;
    }
    let digest = Sha256::digest(verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(digest);
    computed.as_bytes().ct_eq(challenge.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        // The canonical example from RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(verify_s256(verifier, challenge));
    }

    #[test]
    fn wrong_verifier_fails() {
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(!verify_s256(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            challenge
        ));
    }

    #[test]
    fn rejects_short_or_long_verifier() {
        assert!(!is_valid_verifier("short"));
        assert!(!is_valid_verifier(&"a".repeat(129)));
        assert!(is_valid_verifier(&"a".repeat(43)));
        assert!(is_valid_verifier(&"a".repeat(128)));
    }

    #[test]
    fn rejects_bad_verifier_charset() {
        // 43 chars including a space — right length, wrong alphabet.
        let bad = format!(" {}", "a".repeat(42));
        assert!(!is_valid_verifier(&bad));
    }
}
