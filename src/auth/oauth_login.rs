//! Provider-agnostic OAuth login materialisation (Phase C) plus the shared
//! capped-body fetch used by every provider's Phase B. Three-phase shape:
//!
//! 1. **Phase A** — consume `oauth_states` row in one statement, no upstream
//!    calls yet.
//! 2. **Phase B** — provider module exchanges `code` and fetches the remote
//!    profile into a [`RemoteIdentity`]. No DB connection held.
//! 3. **Phase C** — find-or-create user + identity, auto-create signup org
//!    for new users, all inside a fresh tx (here).

use anyhow::Context;
use chrono::{DateTime, Utc};
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::OauthProvider;
use crate::domain::{OrgId, UserId, generate_signup_slug};
use crate::error::{AppError, Result};
use crate::http_outbound::OutboundHttpClient;
use crate::storage::orgs::create_signup_org_with_owner_in_tx;
use crate::storage::users as users_store;

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub(crate) const UA: &str = "uptimepage/auth";

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Parse an OAuth token-endpoint response body shared by every provider:
/// surface the endpoint's own error pair, then require a non-empty token.
pub(crate) fn parse_access_token(body: &[u8], what: &'static str) -> Result<String> {
    let parsed: TokenResponse = serde_json::from_slice(body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("{what} parse: {e}")))?;
    if let Some(err) = parsed.error {
        let desc = parsed.error_description.unwrap_or_default();
        return Err(AppError::Other(anyhow::anyhow!("{what}: {err} ({desc})")));
    }
    parsed
        .access_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("{what}: empty token")))
}

/// Signup-slug retry budget. `generate_signup_slug` collides at p≈1e-9 per
/// pair; 5 retries covers the 99.9999... case without spinning.
const SIGNUP_SLUG_RETRIES: u32 = 5;

/// Aggregated upstream result of Phase B. `verified_email` must be None
/// unless the provider attested ownership — the email-link guard against
/// account takeover lives in that field, not in Phase C.
#[derive(Debug, Clone)]
pub struct RemoteIdentity {
    pub provider_user_id: String,
    pub provider_username: Option<String>,
    pub verified_email: Option<String>,
    pub display_name: Option<String>,
}

/// Phase C result: the resolved user + the org id their session should land
/// on. `signup_org_id` is the user's oldest active membership — for a
/// brand-new user that's the just-created signup org; for an existing user
/// it's whatever they already had. The callback stuffs this into
/// `session.active_org_id` so every subsequent request resolves a real org
/// without any global "default" fallback.
#[derive(Debug, Clone)]
pub struct ResolvedIdentity {
    pub user_id: UserId,
    pub signup_org_id: Option<OrgId>,
    pub is_new_user: bool,
    /// True when this sign-in un-deleted a soft-deleted account — re-auth IS the
    /// restore. The caller surfaces a "welcome back" notice.
    pub restored: bool,
}

/// Capped-body HTTP fetch shared by the provider modules' Phase B calls.
pub(crate) async fn fetch_limited(
    http: &OutboundHttpClient,
    req: Request<Full<Bytes>>,
    what: &'static str,
) -> Result<bytes::Bytes> {
    let resp = http
        .request(req)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("{what} request: {e}")))?;
    let status = resp.status();
    let limited = Limited::new(resp.into_body(), MAX_RESPONSE_BYTES);
    let collected = limited
        .collect()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("{what} body read: {e}")))?
        .to_bytes();
    if !status.is_success() {
        let snippet = String::from_utf8_lossy(&collected);
        return Err(AppError::Other(anyhow::anyhow!(
            "{what} upstream {status}: {snippet}"
        )));
    }
    Ok(collected)
}

/// Phase C of the callback. Find-or-create the user, link the identity, and —
/// for fresh users — create the signup org plus the owner membership. A
/// soft-deleted account that signs in again is un-deleted in this same tx
/// (re-authentication is the restore). Caller follows with `session::create`.
/// All work runs inside one tx, no upstream calls.
pub async fn upsert_identity_and_signup_org(
    pool: &PgPool,
    provider: OauthProvider,
    identity: &RemoteIdentity,
) -> Result<ResolvedIdentity> {
    let mut tx = pool.begin().await.context("phase C: begin tx")?;

    // deleted_at travels with the lookup so a soft-deleted user is restored
    // (un-deleted in this tx) rather than silently logged in over a tombstone.
    let existing: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT oi.user_id, u.deleted_at \
         FROM oauth_identities oi JOIN users u ON u.id = oi.user_id \
         WHERE oi.provider = $1 AND oi.provider_user_id = $2",
    )
    .bind(provider.as_db_str())
    .bind(&identity.provider_user_id)
    .fetch_optional(&mut *tx)
    .await
    .context("phase C: identity lookup")?;

    if let Some((user_id, deleted_at)) = existing {
        let restored = deleted_at.is_some();
        if restored {
            // Re-auth = restore: un-delete the account + lift its org tombstones
            // in this tx, then log in normally.
            crate::auth::account::undelete_in_tx(&mut tx, UserId(user_id)).await?;
        }
        sqlx::query(
            "UPDATE oauth_identities SET last_login_at = now(), provider_username = $3 \
             WHERE provider = $1 AND provider_user_id = $2",
        )
        .bind(provider.as_db_str())
        .bind(&identity.provider_user_id)
        .bind(&identity.provider_username)
        .execute(&mut *tx)
        .await
        .context("phase C: bump last_login_at")?;
        tx.commit().await.context("phase C: commit (existing)")?;
        // Resolve AFTER commit so a just-restored org's lifted tombstone is
        // visible (resolve_signup_org runs on its own pool connection).
        let signup_org_id = users_store::resolve_signup_org(pool, UserId(user_id)).await?;
        return Ok(ResolvedIdentity {
            user_id: UserId(user_id),
            signup_org_id,
            is_new_user: false,
            restored,
        });
    }

    // 2. Email-based recovery. CITEXT comparison is the load-bearing reason
    //    invitations + users share the same column type.
    let Some(email) = identity.verified_email.as_ref() else {
        return Err(AppError::Other(anyhow::anyhow!(
            "{provider:?} callback: no verified email and no identity match"
        )));
    };

    // ::citext cast is load-bearing — sqlx binds &str as TEXT, which would
    // select the case-sensitive operator. Tombstones included: a verified
    // email proves ownership, so a soft-deleted account reached via a new
    // provider restores instead of spawning a duplicate row (email unique
    // index is partial). Active row first, then newest tombstone.
    let by_email: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, deleted_at FROM users WHERE email = $1::citext \
         ORDER BY (deleted_at IS NULL) DESC, created_at DESC LIMIT 1",
    )
    .bind(email)
    .fetch_optional(&mut *tx)
    .await
    .context("phase C: user-by-email")?;

    if let Some((user_id, deleted_at)) = by_email {
        let restored = deleted_at.is_some();
        if restored {
            crate::auth::account::undelete_in_tx(&mut tx, UserId(user_id)).await?;
        }
        sqlx::query(
            "INSERT INTO oauth_identities (user_id, provider, provider_user_id, provider_username) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (provider, provider_user_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(provider.as_db_str())
        .bind(&identity.provider_user_id)
        .bind(&identity.provider_username)
        .execute(&mut *tx)
        .await
        .context("phase C: link identity")?;
        if sqlx::query("UPDATE users SET email_verified_at = now() WHERE id = $1 AND email_verified_at IS NULL")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .context("phase C: backfill verified_at")?
            .rows_affected() == 0 {
            // Already verified — no-op.
        }
        tx.commit().await.context("phase C: commit (linked)")?;
        // Resolve AFTER commit — a just-lifted org tombstone must be visible
        // (resolve_signup_org runs on its own pool connection).
        let signup_org_id = users_store::resolve_signup_org(pool, UserId(user_id)).await?;
        return Ok(ResolvedIdentity {
            user_id: UserId(user_id),
            signup_org_id,
            is_new_user: false,
            restored,
        });
    }

    // 3. Brand-new user. Insert user, identity, signup org + owner
    //    membership all in this tx so a rollback leaves zero orphans.
    let (new_user_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, display_name, email_verified_at, \
                            terms_version, privacy_version) \
         VALUES ($1, $2, now(), $3, $4) RETURNING id",
    )
    .bind(email)
    .bind(identity.display_name.as_deref())
    .bind(crate::auth::consent::TERMS_VERSION)
    .bind(crate::auth::consent::PRIVACY_VERSION)
    .fetch_one(&mut *tx)
    .await
    .context("phase C: insert user")?;

    sqlx::query(
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id, provider_username) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(new_user_id)
    .bind(provider.as_db_str())
    .bind(&identity.provider_user_id)
    .bind(&identity.provider_username)
    .execute(&mut *tx)
    .await
    .context("phase C: insert identity")?;

    let org_id = create_signup_org_in_tx(&mut tx, UserId(new_user_id)).await?;

    sqlx::query("UPDATE users SET signup_org_id = $1 WHERE id = $2")
        .bind(org_id.0)
        .bind(new_user_id)
        .execute(&mut *tx)
        .await
        .context("phase C: set signup_org_id")?;

    tx.commit().await.context("phase C: commit (new user)")?;
    Ok(ResolvedIdentity {
        user_id: UserId(new_user_id),
        signup_org_id: Some(org_id),
        is_new_user: true,
        restored: false,
    })
}

/// Signup-org creation inside the new-user tx. Delegates to
/// [`create_signup_org_with_owner_in_tx`] — that helper is the single owner
/// of writes to `organizations` / `memberships` / `org_audit_log`. The retry
/// loop here only covers the rare slug collision from the adjective+noun+suffix
/// RNG; the owner-limit bypass is documented there.
async fn create_signup_org_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
) -> Result<OrgId> {
    for _ in 0..SIGNUP_SLUG_RETRIES {
        let slug = generate_signup_slug();
        if let Some(org_id) =
            create_signup_org_with_owner_in_tx(tx, user, &slug, "My status").await?
        {
            return Ok(org_id);
        }
    }
    Err(AppError::Other(anyhow::anyhow!(
        "signup slug retries exhausted ({SIGNUP_SLUG_RETRIES}) — adjective/noun pool too small or RNG broken"
    )))
}
