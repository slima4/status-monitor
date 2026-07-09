//! Magic-link sign-in tokens. Schema landed in v1; the request/verify endpoints
//! are gated by `auth.enabled_methods` and inert until `"magic_link"` is added.
//!
//! Tokens are 32 cryptographically random bytes encoded as 43-char base64url-
//! no-pad. Only the argon2id hash is persisted; presenting the raw token at the
//! verify endpoint is what proves possession.
//!
//! `token_prefix` (first [`TOKEN_PREFIX_LEN`] chars of the raw token) is
//! stored alongside the hash and indexed. The lookup narrows the candidate
//! set to ~1 row via the prefix and argon2-verifies that row — without the
//! prefix every verify request would argon2-hash every live row, a
//! CPU-amplification DoS at any sustained request rate.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::token_hash::{self, slice_prefix};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct MagicLinkRow {
    pub id: Uuid,
    pub email: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub redirect_after: Option<String>,
    pub invitation_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct CreatedMagicLink {
    pub row: MagicLinkRow,
    /// Raw 43-char base64url token — embedded in the verify URL emailed to the
    /// user. Never persisted, never recoverable after this call returns.
    pub token: String,
}

/// INSERT a magic-link row. `expires_at = now() + expiry_minutes`. The raw
/// token only exists in the returned struct; the DB stores its argon2id hash
/// plus a 16-char prefix for indexed lookup.
pub async fn create(
    pool: &PgPool,
    email: &str,
    ip_hash: Option<&str>,
    expiry_minutes: u32,
    redirect_after: Option<&str>,
    invitation_id: Option<Uuid>,
) -> Result<CreatedMagicLink> {
    let raw = token_hash::generate_raw_token();
    let prefix = slice_prefix(&raw).to_string();
    let hash = token_hash::hash(&raw)?;
    let expires_at = Utc::now() + Duration::minutes(i64::from(expiry_minutes));
    let row: (Uuid, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO magic_link_tokens \
             (email, token_hash, token_prefix, expires_at, ip_hash, redirect_after, invitation_id) \
         VALUES ($1::citext, $2, $3, $4, $5, $6, $7) \
         RETURNING id, created_at, expires_at",
    )
    .bind(email)
    .bind(&hash)
    .bind(&prefix)
    .bind(expires_at)
    .bind(ip_hash)
    .bind(redirect_after)
    .bind(invitation_id)
    .fetch_one(pool)
    .await
    .context("magic_link::create")?;

    Ok(CreatedMagicLink {
        row: MagicLinkRow {
            id: row.0,
            email: email.to_string(),
            created_at: row.1,
            expires_at: row.2,
            redirect_after: redirect_after.map(str::to_string),
            invitation_id,
        },
        token: raw,
    })
}

/// First live (unused, unexpired, hash-matching) row for `raw_token`, or
/// `None`. Read-only: does NOT mark the row used. Lookup is bounded by the
/// indexed `token_prefix` (96-bit prefix entropy), then argon2-verified.
/// Shared by [`peek`] and [`consume`].
async fn find_live_candidate(pool: &PgPool, raw_token: &str) -> Result<Option<RawRow>> {
    let prefix = slice_prefix(raw_token);
    let candidates: Vec<RawRow> = sqlx::query_as(
        "SELECT id, email::text AS email, token_hash, created_at, expires_at, \
                redirect_after, invitation_id \
         FROM magic_link_tokens \
         WHERE token_prefix = $1 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(prefix)
    .fetch_all(pool)
    .await
    .context("magic_link: select candidates")?;
    Ok(candidates
        .into_iter()
        .find(|r| token_hash::verify(raw_token, &r.token_hash)))
}

/// Read-only validity check for the GET confirmation page: is there an unused,
/// unexpired token matching `raw_token`? Returns its row WITHOUT marking it
/// used, so a mail link-scanner's prefetch of the confirm page cannot burn the
/// token; authoritative single-use redemption is [`consume`], on the POST.
pub async fn peek(pool: &PgPool, raw_token: &str) -> Result<Option<MagicLinkRow>> {
    Ok(find_live_candidate(pool, raw_token)
        .await?
        .map(RawRow::into_row))
}

/// Find and atomically mark-used the row matching `raw_token`. Returns the
/// row on success; `None` for any of: nothing matched, expired, already used,
/// deleted. Callers must not distinguish; surface a single indistinguishable
/// invalid-link page.
///
/// Mark-used is a follow-up UPDATE keyed by `id` with a `used_at IS NULL`
/// guard so a concurrent verify of the same token loses the race.
pub async fn consume(pool: &PgPool, raw_token: &str) -> Result<Option<MagicLinkRow>> {
    let Some(r) = find_live_candidate(pool, raw_token).await? else {
        return Ok(None);
    };
    let updated = sqlx::query(
        "UPDATE magic_link_tokens SET used_at = now() WHERE id = $1 AND used_at IS NULL",
    )
    .bind(r.id)
    .execute(pool)
    .await
    .context("magic_link::consume: mark used")?;
    if updated.rows_affected() == 0 {
        // Lost the mark-used race; treat as already consumed.
        return Ok(None);
    }
    Ok(Some(r.into_row()))
}

/// Earliest unused row id for `email` whose `created_at` falls inside the
/// last `window_seconds`. The throttle in `/auth/magic-link/request` runs
/// this from inside `tokio::spawn` and only sends the email when the row it
/// just inserted is the winner — so the response handler's timing never
/// depends on rate-limit state (anti-enum), and concurrent requests for the
/// same address resolve to exactly one outgoing email per window.
///
/// `window_seconds = 0` short-circuits to `Ok(None)` (rate-limit disabled).
pub async fn earliest_in_window(
    pool: &PgPool,
    email: &str,
    window_seconds: u32,
) -> Result<Option<Uuid>> {
    if window_seconds == 0 {
        return Ok(None);
    }
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM magic_link_tokens \
         WHERE email = $1::citext \
           AND used_at IS NULL \
           AND created_at > now() - make_interval(secs => $2) \
         ORDER BY created_at ASC, id ASC \
         LIMIT 1",
    )
    .bind(email)
    .bind(i32::try_from(window_seconds).unwrap_or(i32::MAX))
    .fetch_optional(pool)
    .await
    .context("magic_link::earliest_in_window")?;
    Ok(row.map(|(id,)| id))
}

/// Periodic cleanup. Drops rows whose `expires_at` is past, and used rows
/// older than 7 days (a forensic window for "did this token get redeemed?").
pub async fn purge_old(pool: &PgPool) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "DELETE FROM magic_link_tokens \
         WHERE expires_at < now() \
            OR (used_at IS NOT NULL AND used_at < now() - INTERVAL '7 days')",
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

#[derive(Debug, sqlx::FromRow)]
struct RawRow {
    id: Uuid,
    email: String,
    token_hash: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    redirect_after: Option<String>,
    invitation_id: Option<Uuid>,
}

impl RawRow {
    fn into_row(self) -> MagicLinkRow {
        MagicLinkRow {
            id: self.id,
            email: self.email,
            created_at: self.created_at,
            expires_at: self.expires_at,
            redirect_after: self.redirect_after,
            invitation_id: self.invitation_id,
        }
    }
}
