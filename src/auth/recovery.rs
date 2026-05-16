//! Account-recovery tokens for the soft-delete / undo flow.
//!
//! A user who deletes their account is soft-deleted and handed a one-shot
//! recovery link. Same hash discipline as invitations / magic-link: the raw
//! 43-char base64url token is returned once (to build the email) and never
//! persisted; the DB stores only its argon2id hash plus an indexed
//! `token_prefix`. Redeem narrows by prefix, then argon2-verifies the
//! survivor — without the prefix every redeem would argon2-hash every live
//! row, a CPU-amplification DoS.
//!
//! The row is created inside the same transaction that soft-deletes the user
//! (see [`crate::auth::account`]), so a failed deletion never leaves a
//! dangling token. The partial unique index
//! `idx_user_recovery_tokens_one_active_per_user` makes a second concurrent
//! deletion of the same account a unique violation rather than a second valid
//! link.

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::auth::token_hash::{self, slice_prefix};
use crate::domain::UserId;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct RecoveryRow {
    pub id: Uuid,
    pub user_id: UserId,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreatedRecovery {
    pub row: RecoveryRow,
    /// Raw token — embedded in the recovery link only. Never persisted,
    /// never recoverable after this call returns.
    pub token: String,
}

/// INSERT a recovery row **inside the caller's transaction**. The raw token is
/// returned for the email; only its argon2id hash and a 16-char prefix land in
/// the table. `expires_at` is the recovery deadline (== the purge grace
/// boundary, so the window is enforced by data, not by re-deriving the grace
/// period in two places).
///
/// A unique-violation here (`idx_user_recovery_tokens_one_active_per_user`)
/// means the account already has a live recovery token — i.e. it is already
/// soft-deleted. The caller maps that to a clean 4xx instead of minting a
/// second valid link.
pub async fn create_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    expires_at: DateTime<Utc>,
) -> sqlx::Result<CreatedRecovery> {
    let raw = token_hash::generate_raw_token();
    let prefix = slice_prefix(&raw).to_string();
    let hash = token_hash::hash(&raw).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO user_recovery_tokens (user_id, token_hash, token_prefix, expires_at) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id",
    )
    .bind(user_id.0)
    .bind(&hash)
    .bind(&prefix)
    .bind(expires_at)
    .fetch_one(&mut **tx)
    .await?;

    Ok(CreatedRecovery {
        row: RecoveryRow {
            id: row.0,
            user_id,
            expires_at,
        },
        token: raw,
    })
}

/// Resolve a presented raw token to its (still live) row. Narrows by indexed
/// `token_prefix`, then argon2-verifies. Returns `None` for anything that
/// isn't a live token — nothing matched, expired, or already used. The caller
/// must not distinguish these (anti-enumeration); the same opaque error
/// covers them all, exactly as magic-link / invitations do.
///
/// Does **not** mark the row used: the recover handler does that inside the
/// same transaction that clears `users.deleted_at`, so a crash mid-recovery
/// can't burn the only link without actually restoring the account.
pub async fn resolve(pool: &sqlx::PgPool, raw_token: &str) -> Result<Option<RecoveryRow>> {
    let prefix = slice_prefix(raw_token);
    let candidates: Vec<RawRow> = sqlx::query_as(
        "SELECT id, user_id, token_hash, expires_at \
         FROM user_recovery_tokens \
         WHERE token_prefix = $1 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(prefix)
    .fetch_all(pool)
    .await
    .context("recovery::resolve: select candidates")?;

    for r in candidates {
        if token_hash::verify(raw_token, &r.token_hash) {
            return Ok(Some(RecoveryRow {
                id: r.id,
                user_id: UserId(r.user_id),
                expires_at: r.expires_at,
            }));
        }
    }
    Ok(None)
}

#[derive(Debug, sqlx::FromRow)]
struct RawRow {
    id: Uuid,
    user_id: Uuid,
    token_hash: String,
    expires_at: DateTime<Utc>,
}
