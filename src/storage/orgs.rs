//! Org lifecycle and access-control helpers. The `organizations` and
//! `memberships` tables sit outside every tenant-scoped repository — every
//! `SELECT` against them lives here so the access layer for those tables has
//! exactly one owner.

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{OrgId, PERSONAL_SLUG_LIKE_PATTERN, UserId};
use crate::error::{AppError, Result};

/// Find-or-create the default org at startup. Returns the persisted UUID so
/// callers don't need to know whether the row already existed. Using
/// `ON CONFLICT (slug) DO UPDATE SET slug = EXCLUDED.slug` makes the statement
/// always `RETURNING id`, dodging the alternative two-statement
/// `INSERT ... ON CONFLICT DO NOTHING` + `SELECT` shape that races on first
/// boot.
pub async fn ensure_default_org(pool: &PgPool, slug: &str) -> Result<OrgId> {
    let row: (Uuid,) = sqlx::query_as(
        r#"INSERT INTO organizations (slug, name)
           VALUES ($1, 'Default')
           ON CONFLICT (slug) DO UPDATE SET slug = EXCLUDED.slug
           RETURNING id"#,
    )
    .bind(slug)
    .fetch_one(pool)
    .await
    .context("ensure_default_org")?;
    Ok(OrgId(row.0))
}

/// Returns true iff `user` is a current member of `org` *and* `org` is not
/// soft-deleted. Both filters matter:
///
///  * the membership row is the access-control check
///  * `deleted_at IS NULL` closes the "bookmark survives delete" bug — a stale
///    tab pointing at a deleted org's resources must 403/404, not 200.
pub async fn is_active_member(pool: &PgPool, user: UserId, org: OrgId) -> Result<bool> {
    let (exists,): (bool,) = sqlx::query_as(
        r#"SELECT EXISTS (
            SELECT 1 FROM memberships m
            JOIN organizations o ON o.id = m.org_id
            WHERE m.user_id = $1
              AND m.org_id = $2
              AND o.deleted_at IS NULL
        )"#,
    )
    .bind(user.0)
    .bind(org.0)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("is_active_member: {e}")))?;
    Ok(exists)
}

/// Returns the user's auto-generated personal-org id, if it still exists.
///
/// "Personal" is identified by two signals taken together:
///  * slug matches the full generated shape `personal-{adj}-{noun}-{6char}`
///    via [`PERSONAL_SLUG_LIKE_PATTERN`] — a user-named org like
///    `personal-team-x` does *not* match
///  * the user joined as `owner` — invited memberships to someone else's
///    `personal-*` slug do not count
///
/// Picks the oldest matching ownership when more than one is found, which is
/// the row created by the signup transaction.
pub async fn personal_org_for_user(pool: &PgPool, user: UserId) -> Result<Option<OrgId>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"SELECT o.id FROM organizations o
           JOIN memberships m ON m.org_id = o.id
           WHERE m.user_id = $1
             AND m.role = 'owner'
             AND o.deleted_at IS NULL
             AND o.slug LIKE $2
           ORDER BY m.created_at ASC
           LIMIT 1"#,
    )
    .bind(user.0)
    .bind(PERSONAL_SLUG_LIKE_PATTERN)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("personal_org_for_user: {e}")))?;
    Ok(row.map(|(id,)| OrgId(id)))
}
