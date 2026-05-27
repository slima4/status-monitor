//! User-row reads and writes outside the auth/signup path. Sign-up + email
//! verification stay in `auth::account` / `auth::github` because they touch
//! related tables in the same transaction; everything else lives here.

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{AppTheme, OrgId, UserId};
use crate::error::Result;

pub async fn get_theme(pool: &PgPool, user: UserId) -> Result<AppTheme> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT theme FROM users WHERE id = $1 AND deleted_at IS NULL")
            .bind(user.0)
            .fetch_optional(pool)
            .await
            .context("get_theme")?;
    Ok(row
        .map(|(s,)| AppTheme::from_db(&s))
        .unwrap_or(AppTheme::Default))
}

pub async fn set_theme(pool: &PgPool, user: UserId, theme: AppTheme) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE users SET theme = $2, updated_at = now() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user.0)
    .bind(theme.as_str())
    .execute(pool)
    .await
    .context("set_theme")?;
    Ok(res.rows_affected() == 1)
}

/// Returns the signup org only when it (a) is set and (b) still exists.
/// A user who soft-deleted their signup org keeps the stale column value;
/// the join filters it out so callers don't anchor pages on a tombstone.
pub async fn get_signup_org_id(pool: &PgPool, user: UserId) -> Result<Option<OrgId>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT u.signup_org_id FROM users u \
         JOIN organizations o ON o.id = u.signup_org_id \
         WHERE u.id = $1 AND u.deleted_at IS NULL AND o.deleted_at IS NULL",
    )
    .bind(user.0)
    .fetch_optional(pool)
    .await
    .context("get_signup_org_id")?;
    Ok(row.map(|(id,)| OrgId(id)))
}

/// Onboarding anchor / post-login landing org. Prefers the explicit signup
/// column (filtered for live orgs above); falls back to the oldest active
/// membership for invited-only users.
pub async fn resolve_signup_org(pool: &PgPool, user: UserId) -> Result<Option<OrgId>> {
    if let Some(id) = get_signup_org_id(pool, user).await? {
        return Ok(Some(id));
    }
    crate::storage::orgs::oldest_membership_for_user(pool, user).await
}

pub async fn mark_onboarding_complete(pool: &PgPool, user: UserId) -> Result<()> {
    sqlx::query(
        "UPDATE users SET onboarding_completed_at = now(), updated_at = now() \
         WHERE id = $1 AND deleted_at IS NULL AND onboarding_completed_at IS NULL",
    )
    .bind(user.0)
    .execute(pool)
    .await
    .context("mark_onboarding_complete")?;
    Ok(())
}

pub async fn is_onboarding_complete(pool: &PgPool, user: UserId) -> Result<bool> {
    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT onboarding_completed_at IS NOT NULL FROM users \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user.0)
    .fetch_optional(pool)
    .await
    .context("is_onboarding_complete")?;
    Ok(row.map(|(b,)| b).unwrap_or(false))
}
