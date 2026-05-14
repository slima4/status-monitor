//! Org lifecycle helpers. Phase 1 only ships `ensure_default_org`; org CRUD
//! lands when the repository pattern arrives.

use anyhow::Context;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::OrgId;
use crate::error::Result;

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
