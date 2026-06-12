//! Organization invitations: token generation, persistence, accept/decline,
//! cleanup.
//!
//! Tokens are 32 cryptographically random bytes presented as a 43-char
//! base64url-no-pad string in the email link. Only the argon2id hash is
//! persisted; presenting the raw token at the redeem endpoint is what
//! proves possession.
//!
//! `token_prefix` (first [`TOKEN_PREFIX_LEN`] chars of the raw token) is
//! stored alongside the hash and indexed. Lookup narrows the candidate set
//! to ~1 row via the prefix and argon2-verifies the survivor — without it
//! the redeem path is a CPU-amplification DoS at scale (50 verifies per
//! org × N orgs).
//!
//! Invitations are single-use: the same row carries both `accepted_at` and
//! `declined_at`; either being non-NULL takes the row out of the "pending"
//! partial indexes.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::token_hash::{self, slice_prefix};
use crate::domain::{OrgId, Role, UserId};
use crate::error::{AppError, Result};
use crate::storage::locks::{advisory_xact_lock, org_lock_key};

/// Hash-friendly invitation record. `token_hash` is the encoded argon2id
/// PHC string; only the hash leaves this row.
#[derive(Debug, Clone)]
pub struct InvitationRow {
    pub id: Uuid,
    pub org_id: OrgId,
    pub inviter_id: UserId,
    pub email: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub declined_at: Option<DateTime<Utc>>,
}

/// Output of [`create`] — includes the raw token that must be embedded in the
/// outgoing email. Never persisted, never recoverable after this call returns.
#[derive(Debug, Clone)]
pub struct CreatedInvitation {
    pub row: InvitationRow,
    /// Raw 43-char base64url token. Goes into the email link only.
    pub token: String,
}

pub use crate::auth::token_hash::generate_raw_token;

/// Number of pending invitations on the org. The cap is enforced
/// atomically inside `create` against the plan; this helper is read-only
/// reporting.
pub async fn count_pending_for_org(pool: &PgPool, org: OrgId) -> Result<u32> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM invitations \
         WHERE org_id = $1 AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now()",
    )
    .bind(org.0)
    .fetch_one(pool)
    .await
    .context("invitations::count_pending_for_org")?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Already-pending (non-accepted, non-declined, unexpired) invitation for the
/// given (org, email) pair. CITEXT email comparison.
pub async fn exists_pending_for_email(pool: &PgPool, org: OrgId, email: &str) -> Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM invitations \
         WHERE org_id = $1 AND email = $2::citext \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now() \
         LIMIT 1",
    )
    .bind(org.0)
    .bind(email)
    .fetch_optional(pool)
    .await
    .context("invitations::exists_pending_for_email")?;
    Ok(row.is_some())
}

/// Issue an invitation, enforcing the per-email dedupe and the per-org
/// pending cap **atomically**. A per-org advisory lock held for the
/// transaction serialises concurrent invite creation for the same org, so
/// the dedupe and count checks below cannot race their own INSERT — without
/// it both are check-then-act under READ COMMITTED (two requests both see
/// "no duplicate" / "count < max" and both insert, double-sending to one
/// address and overshooting the cap). This mirrors the owner-org cap in
/// `storage::orgs::create_org_with_owner`. `ALREADY_INVITED` /
/// `INVITATIONS_LIMIT` are returned here rather than pre-checked in the
/// handler so there is exactly one place the rule lives.
pub async fn create(
    pool: &PgPool,
    org: OrgId,
    inviter: UserId,
    email: &str,
    role: Role,
    expiry_hours: u32,
    max_pending: u32,
) -> Result<CreatedInvitation> {
    let raw = generate_raw_token();
    let prefix = slice_prefix(&raw).to_string();
    let expires_at = Utc::now() + Duration::hours(i64::from(expiry_hours));

    let mut tx = pool.begin().await.context("invitations::create: begin")?;

    advisory_xact_lock(&mut *tx, &org_lock_key(org))
        .await
        .context("invitations::create: advisory lock")?;

    let duplicate: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM invitations \
         WHERE org_id = $1 AND email = $2::citext \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now() \
         LIMIT 1",
    )
    .bind(org.0)
    .bind(email)
    .fetch_optional(&mut *tx)
    .await
    .context("invitations::create: dedupe")?;
    if duplicate.is_some() {
        tx.rollback().await.ok();
        return Err(AppError::conflict(
            crate::api::error::codes::ALREADY_INVITED,
            "there is already a pending invitation for this email",
        ));
    }

    let (pending,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM invitations \
         WHERE org_id = $1 AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now()",
    )
    .bind(org.0)
    .fetch_one(&mut *tx)
    .await
    .context("invitations::create: count pending")?;
    if u32::try_from(pending).unwrap_or(u32::MAX) >= max_pending {
        tx.rollback().await.ok();
        crate::quotas::service::record_quota_event(
            Some(pool.clone()),
            Some(org),
            Some(inviter),
            "quota_exceeded",
            Some("max_pending_invitations"),
            serde_json::json!({ "current": pending, "limit": i64::from(max_pending) }),
            None,
        );
        return Err(AppError::conflict(
            crate::api::error::codes::INVITATIONS_LIMIT,
            format!("pending invitation limit reached ({max_pending})"),
        ));
    }

    // Argon2 only after the cheap dedupe/cap rejects: hashing first would
    // make every blocked abuse-path request pay ~150 ms of CPU for a token
    // that's discarded — the exact cost the cap exists to bound.
    let hash = token_hash::hash(&raw)?;

    let row: (Uuid, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO invitations \
            (org_id, inviter_id, email, role, token_hash, token_prefix, expires_at) \
         VALUES ($1, $2, $3::citext, $4, $5, $6, $7) \
         RETURNING id, created_at, expires_at",
    )
    .bind(org.0)
    .bind(inviter.0)
    .bind(email)
    .bind(role.as_db_str())
    .bind(&hash)
    .bind(&prefix)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await
    .context("invitations::create: insert")?;

    tx.commit().await.context("invitations::create: commit")?;

    Ok(CreatedInvitation {
        row: InvitationRow {
            id: row.0,
            org_id: org,
            inviter_id: inviter,
            email: email.to_string(),
            role,
            created_at: row.1,
            expires_at: row.2,
            accepted_at: None,
            declined_at: None,
        },
        token: raw,
    })
}

/// Find the unique pending invitation that matches `raw_token`. Narrows the
/// candidate set via the indexed `token_prefix` column (96-bit prefix
/// entropy), then argon2-verifies the surviving rows.
///
/// Returns `None` for any of: nothing matched, expired, accepted/declined,
/// row deleted. The handler must not distinguish these to the caller —
/// "INVITATION_INVALID" covers them all (anti-enumeration).
/// Login-flow edge resolver: raw invitation token (login page query param) →
/// pending row id, trimmed. Unknown/expired tokens fall through to None
/// silently — the post-login redirect just lands at `/` and the operator can
/// re-issue. Single owner of that policy for the OAuth and magic-link starts.
pub async fn resolve_pending_invitation_id(
    pool: &PgPool,
    raw: Option<&str>,
) -> Result<Option<Uuid>> {
    match raw.map(str::trim) {
        Some(t) if !t.is_empty() => Ok(find_pending_by_token(pool, t).await?.map(|r| r.id)),
        _ => Ok(None),
    }
}

/// Pending-row lookup by id — for login flows whose `oauth_states` /
/// `magic_link_tokens` row carries a resolved invitation id (possession was
/// proven at login start, so no argon2 here).
pub async fn find_pending_by_id(pool: &PgPool, id: Uuid) -> Result<Option<InvitationRow>> {
    let row: Option<RawRow> = sqlx::query_as(
        "SELECT id, org_id, inviter_id, email::text AS email, role, token_hash, \
                created_at, expires_at, accepted_at, declined_at \
         FROM invitations \
         WHERE id = $1 \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now()",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("invitations::find_pending_by_id")?;
    row.map(InvitationRow::try_from).transpose()
}

pub async fn find_pending_by_token(
    pool: &PgPool,
    raw_token: &str,
) -> Result<Option<InvitationRow>> {
    let prefix = slice_prefix(raw_token);
    let rows: Vec<RawRow> = sqlx::query_as(
        "SELECT id, org_id, inviter_id, email::text AS email, role, token_hash, \
                created_at, expires_at, accepted_at, declined_at \
         FROM invitations \
         WHERE token_prefix = $1 \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now()",
    )
    .bind(prefix)
    .fetch_all(pool)
    .await
    .context("invitations::find_pending_by_token")?;

    for r in rows {
        if token_hash::verify(raw_token, &r.token_hash) {
            return Ok(Some(InvitationRow::try_from(r)?));
        }
    }
    Ok(None)
}

impl TryFrom<RawRow> for InvitationRow {
    type Error = AppError;

    fn try_from(r: RawRow) -> Result<Self> {
        Ok(Self {
            id: r.id,
            org_id: OrgId(r.org_id),
            inviter_id: UserId(r.inviter_id),
            email: r.email,
            role: Role::from_db_str(&r.role).ok_or_else(|| {
                AppError::Other(anyhow::anyhow!(
                    "invitation row {} has unknown role {}",
                    r.id,
                    r.role
                ))
            })?,
            created_at: r.created_at,
            expires_at: r.expires_at,
            accepted_at: r.accepted_at,
            declined_at: r.declined_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RawRow {
    id: Uuid,
    org_id: Uuid,
    inviter_id: Uuid,
    email: String,
    role: String,
    token_hash: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    accepted_at: Option<DateTime<Utc>>,
    declined_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InvitationListing {
    pub id: Uuid,
    pub email: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub declined_at: Option<DateTime<Utc>>,
}

pub async fn list_pending_for_org(pool: &PgPool, org: OrgId) -> Result<Vec<InvitationListing>> {
    let rows: Vec<InvitationListing> = sqlx::query_as(
        "SELECT id, email::text AS email, role, created_at, expires_at, accepted_at, declined_at \
         FROM invitations \
         WHERE org_id = $1 \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now() \
         ORDER BY created_at DESC",
    )
    .bind(org.0)
    .fetch_all(pool)
    .await
    .context("invitations::list_pending_for_org")?;
    Ok(rows)
}

/// Hard-delete a still-pending invitation by id. The (id, org_id) tuple is
/// required so a sibling-org owner can't revoke another org's invitation.
pub async fn revoke(pool: &PgPool, org: OrgId, id: Uuid) -> Result<bool> {
    let res = sqlx::query(
        "DELETE FROM invitations \
         WHERE id = $1 AND org_id = $2 \
         AND accepted_at IS NULL AND declined_at IS NULL",
    )
    .bind(id)
    .bind(org.0)
    .execute(pool)
    .await
    .context("invitations::revoke")?;
    Ok(res.rows_affected() > 0)
}

/// Mark `accepted_at = now()`. Returns false if the row no longer qualified
/// (already accepted, declined, expired, or deleted) — caller surfaces that
/// as `INVITATION_INVALID`. The (id, org_id) tuple ties the mutation to the
/// org the token resolved to; a request-supplied id alone could never flip a
/// pending invite in another tenant.
pub async fn mark_accepted(pool: &PgPool, org: OrgId, id: Uuid) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE invitations SET accepted_at = now() \
         WHERE id = $1 AND org_id = $2 \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now()",
    )
    .bind(id)
    .bind(org.0)
    .execute(pool)
    .await
    .context("invitations::mark_accepted")?;
    Ok(res.rows_affected() > 0)
}

/// Revert a just-stamped accept whose membership insert lost the advisory-
/// locked seat race — the recipient keeps a redeemable token, matching the
/// "your invitation stays valid" contract on the landing page.
pub async fn unmark_accepted(pool: &PgPool, org: OrgId, id: Uuid) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE invitations SET accepted_at = NULL \
         WHERE id = $1 AND org_id = $2 AND accepted_at IS NOT NULL",
    )
    .bind(id)
    .bind(org.0)
    .execute(pool)
    .await
    .context("invitations::unmark_accepted")?;
    Ok(res.rows_affected() > 0)
}

pub async fn mark_declined(pool: &PgPool, org: OrgId, id: Uuid) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE invitations SET declined_at = now() \
         WHERE id = $1 AND org_id = $2 \
         AND accepted_at IS NULL AND declined_at IS NULL \
         AND expires_at > now()",
    )
    .bind(id)
    .bind(org.0)
    .execute(pool)
    .await
    .context("invitations::mark_declined")?;
    Ok(res.rows_affected() > 0)
}

/// Periodic cleanup: drop rows expired more than `keep_history_days` days ago.
/// Accepted/declined rows older than the same window are also pruned — the UI
/// surfaces recent history only. Days are bound via `make_interval` so the
/// query plan is stable and the bind avoids string concatenation.
pub async fn purge_old(pool: &PgPool, keep_history_days: i64) -> Result<u64> {
    let res = sqlx::query(
        "/* SAFE: cross-tenant retention sweep */ \
         DELETE FROM invitations \
         WHERE (accepted_at IS NOT NULL AND accepted_at < now() - make_interval(days => $1)) \
            OR (declined_at IS NOT NULL AND declined_at < now() - make_interval(days => $1)) \
            OR (accepted_at IS NULL AND declined_at IS NULL \
                AND expires_at < now() - make_interval(days => $1))",
    )
    .bind(i32::try_from(keep_history_days).unwrap_or(i32::MAX))
    .execute(pool)
    .await
    .context("invitations::purge_old")?;
    Ok(res.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_token_is_43_chars_base64url_nopad() {
        let t = generate_raw_token();
        assert_eq!(t.len(), 43);
        assert!(!t.contains('=') && !t.contains('+') && !t.contains('/'));
    }
}
