//! `oauth_states` CRUD: insert before redirect, atomic consume on callback.
//!
//! The `consume` query both deletes and returns in one statement so two
//! concurrent callbacks for the same state cannot both proceed.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use rand::TryRng;
use rand::rngs::SysRng;
use sqlx::PgPool;

use crate::error::{AppError, Result};

const STATE_BYTES: usize = 32;
const STATE_EXPIRY_MINUTES: i64 = 10;

#[derive(Debug, Clone)]
pub struct ConsumedState {
    pub provider: String,
    pub redirect_after: Option<String>,
    pub invitation_token: Option<String>,
}

/// 32 random bytes, base64url-no-pad. Caller stores via [`insert`].
pub fn generate_state() -> String {
    let mut bytes = [0u8; STATE_BYTES];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("SysRng must succeed for OAuth state");
    base64url_no_pad(&bytes)
}

pub async fn insert(
    pool: &PgPool,
    state: &str,
    provider: &str,
    redirect_after: Option<&str>,
    invitation_token: Option<&str>,
) -> Result<DateTime<Utc>> {
    let expires_at = Utc::now() + Duration::minutes(STATE_EXPIRY_MINUTES);
    sqlx::query(
        "INSERT INTO oauth_states (state, provider, redirect_after, invitation_token, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(state)
    .bind(provider)
    .bind(redirect_after)
    .bind(invitation_token)
    .bind(expires_at)
    .execute(pool)
    .await
    .context("oauth_state::insert")?;
    Ok(expires_at)
}

/// DELETE-and-RETURN. Returns `None` when the state never existed or has
/// expired. The callback maps that to `INVALID_STATE` without distinguishing
/// the two cases (anti-enumeration; see AUTH §7.8).
pub async fn consume(pool: &PgPool, state: &str) -> Result<Option<ConsumedState>> {
    let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "DELETE FROM oauth_states \
         WHERE state = $1 AND expires_at > now() \
         RETURNING provider, redirect_after, invitation_token",
    )
    .bind(state)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("oauth_state::consume: {e}")))?;
    Ok(row.map(
        |(provider, redirect_after, invitation_token)| ConsumedState {
            provider,
            redirect_after,
            invitation_token,
        },
    ))
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_state_is_url_safe_nopad() {
        let s = generate_state();
        assert!(!s.contains('+') && !s.contains('/') && !s.contains('='));
        assert!(s.len() >= 40);
    }

    #[test]
    fn generate_state_is_unique_over_many_samples() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for _ in 0..1024 {
            assert!(seen.insert(generate_state()));
        }
    }
}
