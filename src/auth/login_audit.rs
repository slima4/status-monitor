//! Records every authentication attempt — success or failure — to power the
//! user's "recent activity" page and credential-stuffing detection.
//!
//! Writes are best-effort: a failure here must not block a request that has
//! otherwise authenticated correctly (the caller logs the error and proceeds).

use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::UserId;
use crate::error::Result;

#[derive(Debug, Clone, Copy)]
pub enum LoginMethod {
    GithubOauth,
    GoogleOauth,
    MicrosoftOauth,
    GitlabOauth,
    Passkey,
    ApiToken,
    MagicLink,
}

impl LoginMethod {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::GithubOauth => "github_oauth",
            Self::GoogleOauth => "google_oauth",
            Self::MicrosoftOauth => "microsoft_oauth",
            Self::GitlabOauth => "gitlab_oauth",
            Self::Passkey => "passkey",
            Self::ApiToken => "api_token",
            Self::MagicLink => "magic_link",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoginAttempt<'a> {
    pub user_id: Option<UserId>,
    pub success: bool,
    pub ip_hash: Option<&'a str>,
    pub user_agent_hash: Option<&'a str>,
    pub failure_reason: Option<&'a str>,
}

/// Best-effort anonymous-failure write. Centralises the `LoginAttempt {
/// success: false, user_id: None, … }` shape used by every callback's
/// "couldn't identify the user" path. Errors are logged, not surfaced — a
/// missed audit row must not break a downstream auth response.
pub async fn record_failure_anon(
    pool: &PgPool,
    method: LoginMethod,
    ip_hash: Option<&str>,
    user_agent_hash: Option<&str>,
    reason: &'static str,
) {
    let attempt = LoginAttempt {
        user_id: None,
        success: false,
        ip_hash,
        user_agent_hash,
        failure_reason: Some(reason),
    };
    if let Err(err) = record(pool, method, attempt).await {
        tracing::warn!(error = %err, method = %method.as_db_str(), reason, "login_audit anon failure write failed");
    }
}

pub async fn record(pool: &PgPool, method: LoginMethod, attempt: LoginAttempt<'_>) -> Result<Uuid> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO login_attempts \
            (user_id, method, success, ip_hash, user_agent_hash, failure_reason) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(attempt.user_id.map(|u| u.0))
    .bind(method.as_db_str())
    .bind(attempt.success)
    .bind(attempt.ip_hash)
    .bind(attempt.user_agent_hash)
    .bind(attempt.failure_reason)
    .fetch_one(pool)
    .await
    .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("login_audit::record: {e}")))?;
    Ok(id)
}
