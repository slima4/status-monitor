//! User-row reads and writes outside the auth/signup path. Sign-up + email
//! verification stay in `auth::account` / `auth::github` because they touch
//! related tables in the same transaction; everything else lives here.

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{AppTheme, DisplayPrefs, OrgId, TimeFormat, UserId};
use crate::error::Result;
use crate::security::EmailRisk;

/// Any user at all, soft-deleted included, so first-run seeding stays one-shot.
pub async fn any_exist(pool: &PgPool) -> Result<bool> {
    let (exists,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(pool)
        .await
        .context("users::any_exist")?;
    Ok(exists)
}

/// Both display preferences in one row read — used by the login cookie-issue
/// pass and the account page. The per-preference setters below stay separate;
/// each PATCH endpoint only writes its own column.
pub async fn get_display_prefs(pool: &PgPool, user: UserId) -> Result<DisplayPrefs> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT theme, time_format FROM users WHERE id = $1 AND deleted_at IS NULL")
            .bind(user.0)
            .fetch_optional(pool)
            .await
            .context("get_display_prefs")?;
    Ok(match row {
        Some((theme, time_format)) => DisplayPrefs {
            theme: AppTheme::from_db(&theme),
            time_format: TimeFormat::from_db(&time_format),
        },
        None => DisplayPrefs::default(),
    })
}

pub async fn get_time_format(pool: &PgPool, user: UserId) -> Result<TimeFormat> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT time_format FROM users WHERE id = $1 AND deleted_at IS NULL")
            .bind(user.0)
            .fetch_optional(pool)
            .await
            .context("get_time_format")?;
    Ok(row.map(|(s,)| TimeFormat::from_db(&s)).unwrap_or_default())
}

pub async fn set_time_format(pool: &PgPool, user: UserId, fmt: TimeFormat) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE users SET time_format = $2, updated_at = now() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user.0)
    .bind(fmt.as_str())
    .execute(pool)
    .await
    .context("set_time_format")?;
    Ok(res.rows_affected() == 1)
}

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

/// User, their own org and the membership that owns it, in one transaction.
/// `created == false` means a concurrent signup won the partial-unique-index
/// race, so the existing account is returned and no second org is made.
///
/// `risk` records what the address looked like at this moment. It is stamped
/// on the insert rather than derived later because the disposable corpus moves
/// under us — an address listed today may be delisted next week, and churn
/// analysis needs to know which it was when the account opened.
pub async fn create_signup_user(
    pool: &PgPool,
    email: &str,
    risk: Option<EmailRisk>,
) -> Result<(UserId, bool)> {
    let mut tx = pool.begin().await.context("create_signup_user: begin")?;
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO users (email, email_verified_at, terms_version, privacy_version, email_risk) \
         VALUES ($1, now(), $2, $3, $4) \
         ON CONFLICT (email) WHERE deleted_at IS NULL DO NOTHING \
         RETURNING id",
    )
    .bind(email)
    .bind(crate::auth::consent::TERMS_VERSION)
    .bind(crate::auth::consent::PRIVACY_VERSION)
    .bind(risk.map(EmailRisk::as_db_str))
    .fetch_optional(&mut *tx)
    .await
    .context("create_signup_user: insert user")?;
    let Some((user_id,)) = inserted else {
        tx.rollback().await.ok();
        let (id,): (Uuid,) =
            sqlx::query_as("SELECT id FROM users WHERE email = $1::citext AND deleted_at IS NULL")
                .bind(email)
                .fetch_one(pool)
                .await
                .context("create_signup_user: resolve race winner")?;
        return Ok((UserId(id), false));
    };
    let user = UserId(user_id);
    let org_id = crate::storage::orgs::create_signup_org_in_tx(&mut tx, user).await?;
    sqlx::query("UPDATE users SET signup_org_id = $1 WHERE id = $2")
        .bind(org_id.0)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("create_signup_user: set signup_org_id")?;
    tx.commit().await.context("create_signup_user: commit")?;
    Ok((user, true))
}

/// Invited-bootstrap user (magic-link + invitation): verified email, consent
/// stamped, NO personal signup org — their only org is the one they join,
/// resolved via oldest membership. `created == false` means a concurrent
/// bootstrap won the partial-unique-index race; the caller must NOT
/// compensate-delete a row it didn't create.
pub async fn create_invited_user(
    pool: &PgPool,
    email: &str,
    risk: Option<EmailRisk>,
) -> Result<(UserId, bool)> {
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO users (email, email_verified_at, terms_version, privacy_version, email_risk) \
         VALUES ($1, now(), $2, $3, $4) \
         ON CONFLICT (email) WHERE deleted_at IS NULL DO NOTHING \
         RETURNING id",
    )
    .bind(email)
    .bind(crate::auth::consent::TERMS_VERSION)
    .bind(crate::auth::consent::PRIVACY_VERSION)
    .bind(risk.map(EmailRisk::as_db_str))
    .fetch_optional(pool)
    .await
    .context("users::create_invited_user")?;
    if let Some((id,)) = inserted {
        return Ok((UserId(id), true));
    }
    let (id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM users WHERE email = $1::citext AND deleted_at IS NULL")
            .bind(email)
            .fetch_one(pool)
            .await
            .context("users::create_invited_user: race-winner lookup")?;
    Ok((UserId(id), false))
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
