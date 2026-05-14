//! Argon2id password-hash helpers shared by the API-token (`§4.4`) and the
//! invitation/magic-link token flows.
//!
//! Single owner of these primitives. If argon2 params ever need tuning
//! (memory cost, parallelism), one place changes — the two flows can't
//! silently drift.

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

use crate::error::{AppError, Result};

/// Hash a token with argon2id (default params). Returns the encoded PHC
/// string suitable for direct INSERT into a `token_hash` column.
pub fn hash(raw: &str) -> Result<String> {
    Ok(Argon2::default()
        .hash_password(raw.as_bytes())
        .map_err(|e| AppError::Other(anyhow::anyhow!("argon2 hash: {e}")))?
        .to_string())
}

/// Constant-time verify of a presented token against the encoded PHC string.
pub fn verify(raw: &str, encoded: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(encoded) else {
        return false;
    };
    Argon2::default()
        .verify_password(raw.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let raw = "sm_live_abc123_some_random_token_value_here";
        let h = hash(raw).unwrap();
        assert!(verify(raw, &h));
        assert!(!verify("other", &h));
    }
}
