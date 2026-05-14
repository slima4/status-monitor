//! Org lifecycle and access-control helpers. The `organizations` and
//! `memberships` tables sit outside every tenant-scoped repository — every
//! `SELECT` against them lives here so the access layer for those tables has
//! exactly one owner.

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{Membership, OrgId, Organization, PERSONAL_SLUG_LIKE_PATTERN, Role, UserId};
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

/// Returns true if the slug is currently free — i.e. no row in
/// `organizations` holds it, including soft-deleted ones. Mirrors the unique
/// index behaviour so a "check-slug" API answer and the actual insert agree.
pub async fn slug_is_available(pool: &PgPool, slug: &str) -> Result<bool> {
    let (exists,): (bool,) =
        sqlx::query_as(r#"SELECT EXISTS (SELECT 1 FROM organizations WHERE slug = $1)"#)
            .bind(slug)
            .fetch_one(pool)
            .await
            .context("slug_is_available")?;
    Ok(!exists)
}

/// Atomic create: organisation row + `owner` membership for the caller, in one
/// transaction. Enforces the per-user owner-org limit inside the same statement
/// that inserts the membership row, so two concurrent creates cannot exceed the
/// cap. The limit counts only **active** owner memberships — soft-deleted orgs
/// during their grace period don't count, which matches what
/// [`owner_org_count`] reports. Returns `Ok(None)` if the slug is already held
/// (including by a soft-deleted org).
pub async fn create_org_with_owner(
    pool: &PgPool,
    user: UserId,
    slug: &str,
    name: &str,
    owner_limit: u32,
) -> Result<Option<Organization>> {
    let mut tx = pool.begin().await.context("create_org_with_owner: begin")?;

    let row: Option<OrgRow> = sqlx::query_as(
        r#"INSERT INTO organizations (slug, name)
           VALUES ($1, $2)
           ON CONFLICT (slug) DO NOTHING
           RETURNING id, slug::text AS slug, name, created_at, updated_at, deleted_at"#,
    )
    .bind(slug)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await
    .context("create_org_with_owner: insert organization")?;

    let Some(org_row) = row else {
        tx.rollback().await.ok();
        return Ok(None);
    };

    let inserted: Option<(Uuid,)> = sqlx::query_as(
        r#"INSERT INTO memberships (user_id, org_id, role)
           SELECT $1, $2, 'owner'
           WHERE (
               SELECT count(*) FROM memberships m
               JOIN organizations o ON o.id = m.org_id
               WHERE m.user_id = $1 AND m.role = 'owner' AND o.deleted_at IS NULL
           ) < $3
           RETURNING org_id"#,
    )
    .bind(user.0)
    .bind(org_row.id)
    .bind(i64::from(owner_limit))
    .fetch_optional(&mut *tx)
    .await
    .context("create_org_with_owner: insert membership")?;

    if inserted.is_none() {
        tx.rollback().await.ok();
        return Err(AppError::unprocessable(
            crate::api::error::codes::OWNER_ORG_LIMIT,
            format!("user already owns the limit of {owner_limit} organizations"),
        ));
    }

    record_audit_tx(
        &mut tx,
        OrgId(org_row.id),
        Some(user),
        "org.created",
        Value::Null,
    )
    .await
    .context("create_org_with_owner: audit")?;

    tx.commit().await.context("create_org_with_owner: commit")?;
    Ok(Some(org_row.into_org()))
}

/// Counts a user's currently-active owner memberships (soft-deleted orgs do
/// not count). Matches the filter used by the atomic enforcer in
/// [`create_org_with_owner`], so a pre-flight "you can create another org"
/// check agrees with what the insert will allow.
pub async fn owner_org_count(pool: &PgPool, user: UserId) -> Result<u32> {
    let (count,): (i64,) = sqlx::query_as(
        r#"SELECT count(*) FROM memberships m
           JOIN organizations o ON o.id = m.org_id
           WHERE m.user_id = $1 AND m.role = 'owner' AND o.deleted_at IS NULL"#,
    )
    .bind(user.0)
    .fetch_one(pool)
    .await
    .context("owner_org_count")?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Find one org by id. Returns soft-deleted rows too — callers that need to
/// hide them (most user-facing GETs) check `deleted_at` themselves.
pub async fn get_org(pool: &PgPool, org: OrgId) -> Result<Option<Organization>> {
    let row: Option<OrgRow> = sqlx::query_as(
        r#"SELECT id, slug::text AS slug, name, created_at, updated_at, deleted_at
           FROM organizations WHERE id = $1"#,
    )
    .bind(org.0)
    .fetch_optional(pool)
    .await
    .context("get_org")?;
    Ok(row.map(OrgRow::into_org))
}

/// Active (non-soft-deleted) orgs the user belongs to, oldest membership first
/// so the picker has a stable order.
pub async fn list_orgs_for_user(pool: &PgPool, user: UserId) -> Result<Vec<OrgWithRole>> {
    let rows: Vec<OrgWithRoleRow> = sqlx::query_as(
        r#"SELECT o.id, o.slug::text AS slug, o.name, o.created_at, o.updated_at,
                  o.deleted_at, m.role
           FROM organizations o
           JOIN memberships m ON m.org_id = o.id
           WHERE m.user_id = $1 AND o.deleted_at IS NULL
           ORDER BY m.created_at ASC"#,
    )
    .bind(user.0)
    .fetch_all(pool)
    .await
    .context("list_orgs_for_user")?;
    Ok(rows.into_iter().map(OrgWithRoleRow::into_dto).collect())
}

/// Soft-deleted orgs the caller is recorded as having deleted, via the latest
/// `org.deleted` audit-log entry per org. Drives the "restore deleted
/// organization" UI in account settings.
pub async fn list_deleted_orgs_deleted_by(
    pool: &PgPool,
    user: UserId,
) -> Result<Vec<Organization>> {
    let rows: Vec<OrgRow> = sqlx::query_as(
        r#"SELECT o.id, o.slug::text AS slug, o.name, o.created_at, o.updated_at, o.deleted_at
           FROM organizations o
           WHERE o.deleted_at IS NOT NULL
             AND $1 = (
                 SELECT al.actor_id FROM org_audit_log al
                 WHERE al.org_id = o.id AND al.action = 'org.deleted'
                 ORDER BY al.occurred_at DESC LIMIT 1
             )
           ORDER BY o.deleted_at DESC"#,
    )
    .bind(user.0)
    .fetch_all(pool)
    .await
    .context("list_deleted_orgs_deleted_by")?;
    Ok(rows.into_iter().map(OrgRow::into_org).collect())
}

/// True iff `user` has the `owner` role on `org` (regardless of soft-delete
/// state). Used by routes that operate on soft-deleted orgs (e.g. restore),
/// where [`is_active_member`]'s `deleted_at IS NULL` filter would mask the row.
pub async fn is_owner(pool: &PgPool, user: UserId, org: OrgId) -> Result<bool> {
    let (exists,): (bool,) = sqlx::query_as(
        r#"SELECT EXISTS (
            SELECT 1 FROM memberships
            WHERE user_id = $1 AND org_id = $2 AND role = 'owner'
        )"#,
    )
    .bind(user.0)
    .bind(org.0)
    .fetch_one(pool)
    .await
    .context("is_owner")?;
    Ok(exists)
}

/// Combined access-control resolution for routes gated on "active owner of
/// this org". One round-trip returns enough to map to 404 (no membership /
/// soft-deleted org) or 403 (member but not owner) without two separate
/// queries. The split helpers ([`is_active_member`] / [`is_owner`]) stay
/// available for paths that need only one signal.
pub async fn membership_status(
    pool: &PgPool,
    user: UserId,
    org: OrgId,
) -> Result<MembershipStatus> {
    let row: Option<(String, bool)> = sqlx::query_as(
        r#"SELECT m.role, o.deleted_at IS NULL AS active
           FROM memberships m
           JOIN organizations o ON o.id = m.org_id
           WHERE m.user_id = $1 AND m.org_id = $2"#,
    )
    .bind(user.0)
    .bind(org.0)
    .fetch_optional(pool)
    .await
    .context("membership_status")?;
    Ok(match row {
        None => MembershipStatus::None,
        Some((_, false)) => MembershipStatus::None,
        Some((role, true)) => match role.as_str() {
            "owner" => MembershipStatus::Owner,
            _ => MembershipStatus::Member,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipStatus {
    Owner,
    Member,
    /// No membership row, or the org is soft-deleted. Cloak as 404.
    None,
}

/// Rename an org. Returns the updated row, `None` if the org doesn't exist or
/// is soft-deleted (operations on deleted orgs are reserved to restore).
/// Writes an `org.renamed` audit row in the same transaction so the renamer
/// is recorded next to the change.
pub async fn update_org_name(
    pool: &PgPool,
    org: OrgId,
    actor: UserId,
    new_name: &str,
) -> Result<Option<Organization>> {
    let mut tx = pool.begin().await.context("update_org_name: begin")?;
    let row: Option<OrgRow> = sqlx::query_as(
        r#"UPDATE organizations
           SET name = $2, updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL
           RETURNING id, slug::text AS slug, name, created_at, updated_at, deleted_at"#,
    )
    .bind(org.0)
    .bind(new_name)
    .fetch_optional(&mut *tx)
    .await
    .context("update_org_name: update")?;
    let Some(row) = row else {
        tx.rollback().await.ok();
        return Ok(None);
    };
    record_audit_tx(
        &mut tx,
        org,
        Some(actor),
        "org.renamed",
        serde_json::json!({ "name": row.name }),
    )
    .await
    .context("update_org_name: audit")?;
    tx.commit().await.context("update_org_name: commit")?;
    Ok(Some(row.into_org()))
}

/// Soft-delete an org. No-op if already deleted; returns `true` only when the
/// row actually transitioned. Audit row is written in the same transaction so
/// the "who deleted this" lookup in [`list_deleted_orgs_deleted_by`] is
/// authoritative.
pub async fn soft_delete_org(pool: &PgPool, org: OrgId, actor: UserId) -> Result<bool> {
    let mut tx = pool.begin().await.context("soft_delete_org: begin")?;
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"UPDATE organizations
           SET deleted_at = now(), updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL
           RETURNING id"#,
    )
    .bind(org.0)
    .fetch_optional(&mut *tx)
    .await
    .context("soft_delete_org: update")?;
    let Some(_) = row else {
        tx.rollback().await.ok();
        return Ok(false);
    };
    record_audit_tx(&mut tx, org, Some(actor), "org.deleted", Value::Null)
        .await
        .context("soft_delete_org: audit")?;
    tx.commit().await.context("soft_delete_org: commit")?;
    Ok(true)
}

/// Clear `deleted_at` on a soft-deleted org, but only if:
///  * the caller is the user who deleted it (latest `org.deleted` audit entry
///    actor matches `actor`), and
///  * it's still inside the `grace_days` window.
///
/// One UPDATE does the eligibility check + the write atomically; a follow-up
/// SELECT only fires when nothing was updated, to distinguish the three "no"
/// cases for the caller. Returns the restored row from the same transaction
/// so handlers don't re-fetch and race with concurrent mutations.
pub async fn restore_org(
    pool: &PgPool,
    org: OrgId,
    actor: UserId,
    grace_days: u32,
) -> Result<RestoreOutcome> {
    let mut tx = pool.begin().await.context("restore_org: begin")?;
    let updated: Option<OrgRow> = sqlx::query_as(
        r#"UPDATE organizations
           SET deleted_at = NULL, updated_at = now()
           WHERE id = $1
             AND deleted_at IS NOT NULL
             AND deleted_at > now() - ($2::int * INTERVAL '1 day')
             AND $3 = (
                 SELECT al.actor_id FROM org_audit_log al
                 WHERE al.org_id = $1 AND al.action = 'org.deleted'
                 ORDER BY al.occurred_at DESC LIMIT 1
             )
           RETURNING id, slug::text AS slug, name, created_at, updated_at, deleted_at"#,
    )
    .bind(org.0)
    .bind(i64::from(grace_days))
    .bind(actor.0)
    .fetch_optional(&mut *tx)
    .await
    .context("restore_org: update")?;

    if let Some(row) = updated {
        record_audit_tx(&mut tx, org, Some(actor), "org.restored", Value::Null)
            .await
            .context("restore_org: audit")?;
        tx.commit().await.context("restore_org: commit")?;
        return Ok(RestoreOutcome::Restored(row.into_org()));
    }

    // Update was a no-op; figure out which precondition failed for a clean
    // status code. This runs at most once per failed restore — the happy
    // path is one round-trip.
    let row: Option<(Option<DateTime<Utc>>, Option<Uuid>)> = sqlx::query_as(
        r#"SELECT o.deleted_at,
                  (SELECT al.actor_id FROM org_audit_log al
                   WHERE al.org_id = o.id AND al.action = 'org.deleted'
                   ORDER BY al.occurred_at DESC LIMIT 1) AS last_deleter
           FROM organizations o WHERE o.id = $1"#,
    )
    .bind(org.0)
    .fetch_optional(&mut *tx)
    .await
    .context("restore_org: diagnose")?;
    tx.rollback().await.ok();
    Ok(match row {
        None => RestoreOutcome::NotFound,
        Some((None, _)) => RestoreOutcome::NotDeleted,
        Some((Some(deleted_at), _))
            if Utc::now().signed_duration_since(deleted_at).num_days() >= i64::from(grace_days) =>
        {
            RestoreOutcome::WindowExpired
        }
        // Soft-deleted, in window, but caller isn't the deleter → cloak as
        // NotFound so non-deleters never confirm existence.
        Some((Some(_), _)) => RestoreOutcome::NotFound,
    })
}

#[derive(Debug, Clone)]
pub enum RestoreOutcome {
    Restored(Organization),
    NotFound,
    NotDeleted,
    WindowExpired,
}

/// List of (membership, optional display email) for every member of an org,
/// ordered by membership creation time. The email lookup goes through
/// `users`, which is fine because membership and user tables are co-located.
pub async fn list_members(pool: &PgPool, org: OrgId) -> Result<Vec<MemberView>> {
    let rows: Vec<MemberRow> = sqlx::query_as(
        r#"SELECT m.user_id, m.role, m.created_at, u.email::text AS email
           FROM memberships m
           JOIN users u ON u.id = m.user_id
           WHERE m.org_id = $1
           ORDER BY m.created_at ASC"#,
    )
    .bind(org.0)
    .fetch_all(pool)
    .await
    .context("list_members")?;
    Ok(rows
        .into_iter()
        .map(|r| MemberView {
            membership: Membership {
                user_id: UserId(r.user_id),
                org_id: org,
                role: Role::from_db_str(&r.role).unwrap_or(Role::Member),
                created_at: r.created_at,
            },
            email: r.email,
        })
        .collect())
}

/// Remove a member from an org. Refuses to remove the last owner (would leave
/// the org headless). Writes a `member.removed` audit row with the removed
/// user's id in metadata. Returns the outcome so the handler can map to the
/// right HTTP status.
pub async fn remove_member(
    pool: &PgPool,
    org: OrgId,
    actor: UserId,
    user: UserId,
) -> Result<RemoveOutcome> {
    let mut tx = pool.begin().await.context("remove_member: begin")?;
    let row: Option<(String,)> = sqlx::query_as(
        r#"SELECT role FROM memberships
           WHERE org_id = $1 AND user_id = $2 FOR UPDATE"#,
    )
    .bind(org.0)
    .bind(user.0)
    .fetch_optional(&mut *tx)
    .await
    .context("remove_member: select")?;
    let Some((role,)) = row else {
        tx.rollback().await.ok();
        return Ok(RemoveOutcome::NotFound);
    };
    if role == "owner" {
        let (owner_count,): (i64,) = sqlx::query_as(
            r#"SELECT count(*) FROM memberships
               WHERE org_id = $1 AND role = 'owner'"#,
        )
        .bind(org.0)
        .fetch_one(&mut *tx)
        .await
        .context("remove_member: owner_count")?;
        if owner_count <= 1 {
            tx.rollback().await.ok();
            return Ok(RemoveOutcome::LastOwner);
        }
    }
    sqlx::query(r#"DELETE FROM memberships WHERE org_id = $1 AND user_id = $2"#)
        .bind(org.0)
        .bind(user.0)
        .execute(&mut *tx)
        .await
        .context("remove_member: delete")?;
    record_audit_tx(
        &mut tx,
        org,
        Some(actor),
        "member.removed",
        serde_json::json!({ "user_id": user.0 }),
    )
    .await
    .context("remove_member: audit")?;
    tx.commit().await.context("remove_member: commit")?;
    Ok(RemoveOutcome::Removed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed,
    NotFound,
    LastOwner,
}

#[derive(Debug, Clone)]
pub struct OrgWithRole {
    pub org: Organization,
    pub role: Role,
}

#[derive(Debug, Clone)]
pub struct MemberView {
    pub membership: Membership,
    pub email: String,
}

async fn record_audit_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: OrgId,
    actor: Option<UserId>,
    action: &str,
    metadata: Value,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO org_audit_log (org_id, actor_id, action, metadata)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(org.0)
    .bind(actor.map(|u| u.0))
    .bind(action)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .context("record_audit_tx")?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct OrgRow {
    id: Uuid,
    slug: String,
    name: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

impl OrgRow {
    fn into_org(self) -> Organization {
        Organization {
            id: OrgId(self.id),
            slug: self.slug,
            name: self.name,
            created_at: self.created_at,
            updated_at: self.updated_at,
            deleted_at: self.deleted_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct OrgWithRoleRow {
    id: Uuid,
    slug: String,
    name: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    role: String,
}

impl OrgWithRoleRow {
    fn into_dto(self) -> OrgWithRole {
        let role = Role::from_db_str(&self.role).unwrap_or(Role::Member);
        OrgWithRole {
            org: Organization {
                id: OrgId(self.id),
                slug: self.slug,
                name: self.name,
                created_at: self.created_at,
                updated_at: self.updated_at,
                deleted_at: self.deleted_at,
            },
            role,
        }
    }
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    user_id: Uuid,
    role: String,
    created_at: DateTime<Utc>,
    email: String,
}
