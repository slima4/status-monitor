//! User-row reads and writes outside the auth/signup path. Sign-up + email
//! verification stay in `auth::account` / `auth::github` because they touch
//! related tables in the same transaction; everything else lives here.

use anyhow::Context;
use sqlx::PgPool;

use crate::domain::{AppTheme, UserId};
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
