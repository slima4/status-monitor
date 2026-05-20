//! GitHub OAuth callback orchestration. Strict three-phase shape:
//!
//! 1. **Phase A** — consume `oauth_states` row in one statement, no upstream
//!    calls yet.
//! 2. **Phase B** — exchange `code` for an access token, fetch `/user` and
//!    `/user/emails`. No DB connection held.
//! 3. **Phase C** — find-or-create user + identity, auto-create signup org
//!    for new users, create the session, all inside a fresh tx.
//!
//! Audit writes happen post-commit on their own connection so they never
//! invalidate a freshly committed session.

use anyhow::Context;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::url::url_encode;
use crate::config::GithubOauthConfig;
use crate::domain::{OrgId, UserId, generate_signup_slug};
use crate::error::{AppError, Result};
use crate::http_outbound::OutboundHttpClient;
use crate::storage::orgs::{create_signup_org_with_owner_in_tx, default_org_for_user};

const GH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GH_USER_URL: &str = "https://api.github.com/user";
const GH_EMAILS_URL: &str = "https://api.github.com/user/emails";
const MAX_GH_RESPONSE_BYTES: usize = 256 * 1024;
const UA: &str = "status-monitor/auth";

/// Signup-slug retry budget. `generate_signup_slug` collides at p≈1e-9 per
/// pair; 5 retries covers the 99.9999... case without spinning.
const SIGNUP_SLUG_RETRIES: u32 = 5;

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
    email: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

/// Aggregated upstream result of Phase B. The callback handler consumes this
/// to materialise the session.
#[derive(Debug, Clone)]
pub struct GithubIdentity {
    pub provider_user_id: String,
    pub provider_username: String,
    pub primary_verified_email: Option<String>,
    pub display_name: Option<String>,
}

/// Build the `https://github.com/login/oauth/authorize` URL with the configured
/// client id, scopes, redirect URI and state. The state must have already been
/// persisted to `oauth_states` before this URL is handed to the user.
pub fn authorize_url(cfg: &GithubOauthConfig, state: &str) -> String {
    let scope = cfg.scopes.join(" ");
    format!(
        "https://github.com/login/oauth/authorize?client_id={cid}&state={st}&scope={sc}&redirect_uri={ru}",
        cid = url_encode(&cfg.client_id),
        st = url_encode(state),
        sc = url_encode(&scope),
        ru = url_encode(&cfg.redirect_url),
    )
}

/// Phase B of the callback — exchange code, fetch profile + verified email.
/// Holds NO database connection across these three calls.
pub async fn fetch_identity(
    http: &OutboundHttpClient,
    cfg: &GithubOauthConfig,
    code: &str,
) -> Result<GithubIdentity> {
    let token = exchange_code(http, cfg, code).await?;
    let user = fetch_user(http, &token).await?;
    let primary = fetch_primary_verified_email(http, &token)
        .await
        .ok()
        .flatten();
    let email = user.email.or(primary);
    Ok(GithubIdentity {
        provider_user_id: user.id.to_string(),
        provider_username: user.login,
        primary_verified_email: email,
        display_name: user.name,
    })
}

async fn exchange_code(
    http: &OutboundHttpClient,
    cfg: &GithubOauthConfig,
    code: &str,
) -> Result<String> {
    let payload = serde_json::to_vec(&json!({
        "client_id": cfg.client_id,
        "client_secret": cfg.client_secret.expose_secret(),
        "code": code,
        "redirect_uri": cfg.redirect_url,
    }))
    .map_err(|e| AppError::Other(anyhow::anyhow!("oauth token body: {e}")))?;
    let req = Request::post(GH_TOKEN_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, UA)
        .body(Full::new(Bytes::from(payload)))
        .map_err(|e| AppError::Other(anyhow::anyhow!("oauth token request: {e}")))?;
    let body = fetch_body(http, req).await?;
    let parsed: TokenResponse = serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("oauth token parse: {e}")))?;
    if let Some(err) = parsed.error {
        let desc = parsed.error_description.unwrap_or_default();
        return Err(AppError::Other(anyhow::anyhow!(
            "github token endpoint: {err} ({desc})"
        )));
    }
    parsed
        .access_token
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("github token endpoint: empty token")))
}

async fn fetch_user(http: &OutboundHttpClient, access_token: &str) -> Result<GithubUser> {
    let req = Request::get(GH_USER_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| AppError::Other(anyhow::anyhow!("github user request: {e}")))?;
    let body = fetch_body(http, req).await?;
    serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("github user parse: {e}")))
}

async fn fetch_primary_verified_email(
    http: &OutboundHttpClient,
    access_token: &str,
) -> Result<Option<String>> {
    let req = Request::get(GH_EMAILS_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| AppError::Other(anyhow::anyhow!("github emails request: {e}")))?;
    let body = fetch_body(http, req).await?;
    let emails: Vec<GithubEmail> = serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("github emails parse: {e}")))?;
    Ok(emails
        .into_iter()
        .find(|e| e.primary && e.verified)
        .map(|e| e.email))
}

async fn fetch_body(http: &OutboundHttpClient, req: Request<Full<Bytes>>) -> Result<bytes::Bytes> {
    let resp = http
        .request(req)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("github request: {e}")))?;
    let status = resp.status();
    let limited = Limited::new(resp.into_body(), MAX_GH_RESPONSE_BYTES);
    let collected = limited
        .collect()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("github body read: {e}")))?
        .to_bytes();
    if !status.is_success() {
        let snippet = String::from_utf8_lossy(&collected);
        return Err(AppError::Other(anyhow::anyhow!(
            "github upstream {status}: {snippet}"
        )));
    }
    Ok(collected)
}

/// Phase C result: the resolved user + the org id their session should land
/// on. `default_org_id` is the user's oldest active membership — for a
/// brand-new user that's the just-created signup org; for an existing user
/// it's whatever they already had. The callback stuffs this into
/// `session.active_org_id` so the next request never falls through to the
/// (now-deleted) slug-shape inference.
#[derive(Debug, Clone)]
pub struct ResolvedIdentity {
    pub user_id: UserId,
    pub default_org_id: Option<OrgId>,
    pub is_new_user: bool,
}

/// Phase C of the callback. Find-or-create the user, link the identity, and —
/// for fresh users — create the signup org plus the owner membership. Caller
/// is expected to immediately follow this with `session::create` on the same
/// pool. All work runs inside one tx, no upstream calls.
pub async fn upsert_identity_and_signup_org(
    pool: &PgPool,
    identity: &GithubIdentity,
) -> Result<ResolvedIdentity> {
    let mut tx = pool.begin().await.context("phase C: begin tx")?;

    // 1. Identity lookup. (provider, provider_user_id) → user_id.
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT user_id FROM oauth_identities \
         WHERE provider = 'github' AND provider_user_id = $1",
    )
    .bind(&identity.provider_user_id)
    .fetch_optional(&mut *tx)
    .await
    .context("phase C: identity lookup")?;

    if let Some((user_id,)) = existing {
        sqlx::query(
            "UPDATE oauth_identities SET last_login_at = now(), provider_username = $2 \
             WHERE provider = 'github' AND provider_user_id = $1",
        )
        .bind(&identity.provider_user_id)
        .bind(&identity.provider_username)
        .execute(&mut *tx)
        .await
        .context("phase C: bump last_login_at")?;
        let default_org_id = default_org_for_user(pool, UserId(user_id)).await?;
        tx.commit().await.context("phase C: commit (existing)")?;
        return Ok(ResolvedIdentity {
            user_id: UserId(user_id),
            default_org_id,
            is_new_user: false,
        });
    }

    // 2. Email-based recovery. CITEXT comparison is the load-bearing reason
    //    invitations + users share the same column type.
    let Some(email) = identity.primary_verified_email.as_ref() else {
        // No verified email and no identity match — caller must bounce to
        // onboarding (out of Phase 2 scope).
        return Err(AppError::Other(anyhow::anyhow!(
            "github callback: no verified primary email; onboarding path lands in Phase 6"
        )));
    };

    // CITEXT cast is load-bearing: sqlx binds `&str` as TEXT, which selects
    // the case-sensitive `text = text` operator. The `::citext` cast forces
    // the case-insensitive CITEXT operator so "Bob@Example.test" matches
    // "bob@example.test" (the cross-flow email-consistency property).
    let by_email: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE email = $1::citext AND deleted_at IS NULL")
            .bind(email)
            .fetch_optional(&mut *tx)
            .await
            .context("phase C: user-by-email")?;

    if let Some((user_id,)) = by_email {
        sqlx::query(
            "INSERT INTO oauth_identities (user_id, provider, provider_user_id, provider_username) \
             VALUES ($1, 'github', $2, $3) \
             ON CONFLICT (provider, provider_user_id) DO NOTHING",
        )
        .bind(user_id)
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
        let default_org_id = default_org_for_user(pool, UserId(user_id)).await?;
        tx.commit().await.context("phase C: commit (linked)")?;
        return Ok(ResolvedIdentity {
            user_id: UserId(user_id),
            default_org_id,
            is_new_user: false,
        });
    }

    // 3. Brand-new user. Insert user, identity, signup org + owner
    //    membership all in this tx so a rollback leaves zero orphans.
    let (new_user_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, display_name, email_verified_at) \
         VALUES ($1, $2, now()) RETURNING id",
    )
    .bind(email)
    .bind(identity.display_name.as_deref())
    .fetch_one(&mut *tx)
    .await
    .context("phase C: insert user")?;

    sqlx::query(
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id, provider_username) \
         VALUES ($1, 'github', $2, $3)",
    )
    .bind(new_user_id)
    .bind(&identity.provider_user_id)
    .bind(&identity.provider_username)
    .execute(&mut *tx)
    .await
    .context("phase C: insert identity")?;

    let org_id = create_signup_org_in_tx(&mut tx, UserId(new_user_id)).await?;

    tx.commit().await.context("phase C: commit (new user)")?;
    Ok(ResolvedIdentity {
        user_id: UserId(new_user_id),
        default_org_id: Some(org_id),
        is_new_user: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_encodes_scope_and_redirect() {
        let cfg = GithubOauthConfig {
            client_id: "cid".into(),
            redirect_url: "https://app.example.test/cb?next=/".into(),
            scopes: vec!["user:email".into(), "read:user".into()],
            ..Default::default()
        };
        let url = authorize_url(&cfg, "abc&def");
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("state=abc%26def"));
        // x-www-form-urlencoded encodes space as `+`, not %20.
        assert!(url.contains("scope=user%3Aemail+read%3Auser"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.example.test%2Fcb%3Fnext%3D%2F"));
    }
}
