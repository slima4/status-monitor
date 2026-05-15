//! `sm_live_` API token issuance, storage and lookup.
//!
//! Format: `sm_live_<43 chars>` where the 43 chars are 32 cryptographically
//! random bytes encoded as base64url (no padding). 51 chars total. The
//! `sm_live_` prefix lets git secret scanners detect leaks; the visible
//! `token_prefix` column stores the first `prefix_visible_chars` characters of
//! the raw token (default 16: `sm_live_` + 8 random base64url chars = 48 bits
//! of prefix entropy).
//!
//! The prefix column is intentionally **not** UNIQUE. Collisions at 48 bits
//! are rare but a UNIQUE constraint would turn the rare event into a 500.
//! Instead the prefix narrows the lookup set and argon2-verify against
//! `token_hash` disambiguates the survivor.

use std::time::Duration as StdDuration;

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use moka::sync::Cache;
use rand::TryRng;
use rand::rngs::SysRng;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::session::LAST_USED_DEBOUNCE_SECS;
use crate::auth::token_hash;
use crate::domain::UserId;
use crate::error::Result;

/// Public prefix that triggers Bearer authentication. Both halves are checked:
/// `Authorization: Bearer <s>` with `s.starts_with(TOKEN_PREFIX)` routes to
/// the API-token path; everything else falls through to session auth.
pub const TOKEN_PREFIX: &str = "sm_live_";

/// Bytes of randomness in a generated token. 32 bytes → 43 base64url chars
/// after the fixed prefix → 51 char total length.
const TOKEN_RANDOM_BYTES: usize = 32;

/// Minimum acceptable value of `prefix_visible_chars`. Below this, the prefix
/// supplies fewer than 48 bits of entropy and the bounded-row-set guarantee
/// behind "lookup is fast and never 500s on collision" breaks down.
pub const MIN_PREFIX_VISIBLE_CHARS: usize = 16;

/// Outcome of an attempted Bearer lookup. The middleware translates `Invalid`
/// into 401; `Active` into populating `AuthContext::ApiToken`.
#[derive(Debug)]
pub enum LookupOutcome {
    Active(ApiTokenRow),
    Invalid,
}

#[derive(Debug, Clone)]
pub struct ApiTokenRow {
    pub id: Uuid,
    pub user_id: UserId,
    pub name: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// One row returned by [`list_for_user`]. `token_prefix` is the only piece of
/// the raw token persisted in plaintext — safe to show in UI.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiTokenListing {
    pub id: Uuid,
    pub name: String,
    pub token_prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Output of [`create`]. `token` is the only place the caller can see the raw
/// value — never persisted, never recoverable.
#[derive(Debug, Clone)]
pub struct CreatedToken {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
}

/// In-process "I wrote `last_used_at` for this token recently" set. Mirrors
/// the session debounce shape — one cache per replica.
pub type ApiTokenLastUsedDebounce = Cache<Uuid, DateTime<Utc>>;

/// Build the API-token `last_used_at` debounce cache. Capacity matches the
/// session cache: well above the realistic active-token set.
pub fn build_debounce_cache() -> ApiTokenLastUsedDebounce {
    Cache::builder()
        .max_capacity(100_000)
        .time_to_live(StdDuration::from_secs(LAST_USED_DEBOUNCE_SECS * 4))
        .build()
}

/// Generate a fresh raw token. 32 random bytes → 43 chars base64url → prefixed
/// with `sm_live_`.
pub fn generate_raw() -> String {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("SysRng must succeed for api-token generation");
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Slice the visible prefix per `prefix_visible_chars`. Panics in debug if
/// the configured value is shorter than [`MIN_PREFIX_VISIBLE_CHARS`] — startup
/// asserts the bound, but the slice would otherwise silently produce a too-
/// short prefix.
fn slice_prefix(raw: &str, prefix_visible_chars: usize) -> &str {
    debug_assert!(
        prefix_visible_chars >= MIN_PREFIX_VISIBLE_CHARS,
        "prefix_visible_chars must be >= {MIN_PREFIX_VISIBLE_CHARS}"
    );
    let n = prefix_visible_chars.min(raw.len());
    &raw[..n]
}

/// Count of non-deleted tokens owned by `user`. The per-user cap is
/// enforced atomically inside `create` against the plan; this helper is
/// read-only reporting.
pub async fn count_for_user(pool: &PgPool, user: UserId) -> Result<u32> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_tokens WHERE user_id = $1")
        .bind(user.0)
        .fetch_one(pool)
        .await
        .context("api_tokens::count_for_user")?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Insert a freshly-issued token. Returns the raw token string — the only
/// place the caller can ever see it. The persisted row holds the argon2id
/// hash plus the visible prefix.
/// `max_tokens` is the plan's `max_api_tokens_per_user`. A per-user
/// advisory lock serialises concurrent creates for the same user so the
/// count + INSERT cannot race (the same standard as the owner-org and
/// invitation caps — not check-then-act). Argon2 runs only *after* the
/// cheap cap reject so a blocked abuse path doesn't pay ~150 ms of CPU.
pub async fn create(
    pool: &PgPool,
    user: UserId,
    name: &str,
    prefix_visible_chars: usize,
    max_tokens: i64,
) -> Result<CreatedToken> {
    let raw = generate_raw();
    let prefix = slice_prefix(&raw, prefix_visible_chars).to_string();

    let mut tx = pool.begin().await.context("api_tokens::create: begin")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(user.0.to_string())
        .execute(&mut *tx)
        .await
        .context("api_tokens::create: advisory lock")?;

    let (current,): (i64,) = sqlx::query_as("SELECT count(*) FROM api_tokens WHERE user_id = $1")
        .bind(user.0)
        .fetch_one(&mut *tx)
        .await
        .context("api_tokens::create: count")?;
    if current + 1 > max_tokens {
        tx.rollback().await.ok();
        return Err(crate::error::AppError::quota_exceeded(
            "max_api_tokens_per_user",
            current,
            max_tokens,
            "free",
        ));
    }

    let hash = token_hash::hash(&raw)?;
    let (id, created_at): (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO api_tokens (user_id, name, token_hash, token_prefix) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id, created_at",
    )
    .bind(user.0)
    .bind(name)
    .bind(&hash)
    .bind(&prefix)
    .fetch_one(&mut *tx)
    .await
    .context("api_tokens::create")?;
    tx.commit().await.context("api_tokens::create: commit")?;
    Ok(CreatedToken {
        id,
        name: name.to_string(),
        token: raw,
        prefix,
        created_at,
    })
}

/// List the caller's tokens. Prefix-only; the raw token can never be
/// reconstructed from this output.
pub async fn list_for_user(pool: &PgPool, user: UserId) -> Result<Vec<ApiTokenListing>> {
    let rows: Vec<ApiTokenListing> = sqlx::query_as(
        "SELECT id, name, token_prefix, created_at, last_used_at, expires_at \
         FROM api_tokens WHERE user_id = $1 \
         ORDER BY created_at DESC",
    )
    .bind(user.0)
    .fetch_all(pool)
    .await
    .context("api_tokens::list_for_user")?;
    Ok(rows)
}

/// Look up a presented token. Strips the `sm_live_` prefix-check, derives the
/// visible prefix per config, fetches candidate rows (bounded by the
/// per-user token cap), and argon2-verifies each. Verification is
/// constant-time; the loop runs at most that many iterations.
pub async fn lookup_by_raw(
    pool: &PgPool,
    raw: &str,
    prefix_visible_chars: usize,
) -> Result<LookupOutcome> {
    if !raw.starts_with(TOKEN_PREFIX) {
        return Ok(LookupOutcome::Invalid);
    }
    let prefix = slice_prefix(raw, prefix_visible_chars);

    type CandidateRow = (Uuid, Uuid, String, String, Option<DateTime<Utc>>);
    let rows: Vec<CandidateRow> = sqlx::query_as(
        "SELECT id, user_id, name, token_hash, expires_at \
         FROM api_tokens WHERE token_prefix = $1",
    )
    .bind(prefix)
    .fetch_all(pool)
    .await
    .context("api_tokens::lookup_by_raw")?;

    let now = Utc::now();
    for (id, user_id, name, hash, expires_at) in rows {
        if expires_at.is_some_and(|e| e <= now) {
            continue;
        }
        if token_hash::verify(raw, &hash) {
            return Ok(LookupOutcome::Active(ApiTokenRow {
                id,
                user_id: UserId(user_id),
                name,
                expires_at,
            }));
        }
    }
    Ok(LookupOutcome::Invalid)
}

/// Synchronous check: would a `touch_last_used_debounced` call actually
/// issue an UPDATE right now? Used by the Bearer middleware to skip the
/// `tokio::spawn` on the no-op path (which is >99% of requests).
pub fn should_touch(cache: &ApiTokenLastUsedDebounce, token_id: Uuid) -> bool {
    match cache.get(&token_id) {
        None => true,
        Some(last) => {
            Utc::now().signed_duration_since(last)
                >= chrono::Duration::seconds(LAST_USED_DEBOUNCE_SECS as i64)
        }
    }
}

/// Lazy `last_used_at` bump. Debounced same as sessions — at most one UPDATE
/// per token per replica per [`LAST_USED_DEBOUNCE_SECS`]. Callers can call
/// [`should_touch`] first to elide the async hop entirely.
pub async fn touch_last_used_debounced(
    pool: &PgPool,
    cache: &ApiTokenLastUsedDebounce,
    token_id: Uuid,
) -> Result<()> {
    if !should_touch(cache, token_id) {
        return Ok(());
    }
    sqlx::query("UPDATE api_tokens SET last_used_at = now() WHERE id = $1")
        .bind(token_id)
        .execute(pool)
        .await
        .context("api_tokens::touch_last_used_debounced")?;
    cache.insert(token_id, Utc::now());
    Ok(())
}

/// Rename the caller's token. Match `user_id` to prevent cross-user rewrites
/// (defence in depth — the handler already extracts the caller).
pub async fn rename_for_user(
    pool: &PgPool,
    user: UserId,
    token_id: Uuid,
    new_name: &str,
) -> Result<bool> {
    let res = sqlx::query("UPDATE api_tokens SET name = $3 WHERE id = $1 AND user_id = $2")
        .bind(token_id)
        .bind(user.0)
        .bind(new_name)
        .execute(pool)
        .await
        .context("api_tokens::rename_for_user")?;
    Ok(res.rows_affected() > 0)
}

/// Targeted revoke. Bound to `user_id` so a user can never destroy another
/// user's token even if they brute-force the id.
pub async fn delete_for_user(pool: &PgPool, user: UserId, token_id: Uuid) -> Result<bool> {
    let res = sqlx::query("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2")
        .bind(token_id)
        .bind(user.0)
        .execute(pool)
        .await
        .context("api_tokens::delete_for_user")?;
    Ok(res.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_raw_is_sm_live_prefixed_51_chars() {
        let raw = generate_raw();
        assert!(raw.starts_with(TOKEN_PREFIX));
        assert_eq!(raw.len(), TOKEN_PREFIX.len() + 43);
        assert!(!raw.contains('=') && !raw.contains('+') && !raw.contains('/'));
    }

    #[test]
    fn slice_prefix_returns_first_n_chars() {
        let raw = "sm_live_abcdefghijklmnop";
        assert_eq!(slice_prefix(raw, 16), "sm_live_abcdefgh");
    }
}
