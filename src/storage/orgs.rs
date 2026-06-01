//! Org lifecycle and access-control helpers. The `organizations` and
//! `memberships` tables sit outside every tenant-scoped repository — every
//! `SELECT` against them lives here so the access layer for those tables has
//! exactly one owner.

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{
    Membership, OrgId, Organization, PageRef, PublicOrgBranding, PublicStyle, Role, StatusPageId,
    UserId,
};
use crate::error::{AppError, Result};
use crate::storage::locks::{advisory_xact_lock, org_lock_key, user_lock_key};

/// Returns the id of the single live (non-soft-deleted) organisation, or
/// `None` if zero or more than one exist. Used by the path-based public
/// surface to find "the lone tenant" on a self-host deploy — `Some(id)` only
/// fires when the box has exactly one tenant, so multi-tenant SaaS deploys
/// always see `None` and route via subdomain instead.
pub async fn find_lone_active_org(pool: &PgPool) -> Result<Option<OrgId>> {
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM organizations WHERE deleted_at IS NULL LIMIT 2")
            .fetch_all(pool)
            .await
            .context("find_lone_active_org")?;
    match rows.as_slice() {
        [(id,)] => Ok(Some(OrgId(*id))),
        _ => Ok(None),
    }
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

/// Returns the user's oldest active membership, regardless of role. Used
/// as the fallback when a session has no `active_org_id` (e.g. just-issued
/// login, cleared cookie), and as the onboarding-page anchor right after
/// signup.
///
/// Why "oldest membership, any role":
///  * For a brand-new user the signup transaction created exactly one
///    owned org, so that's what wins.
///  * For an invited-only user (no own org), their oldest invitation is
///    a meaningful landing spot — better than 403 with nowhere to go.
///  * Robust against slug rename — does not rely on inferring identity
///    from the slug's shape.
pub async fn oldest_membership_for_user(pool: &PgPool, user: UserId) -> Result<Option<OrgId>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"SELECT m.org_id FROM memberships m
           JOIN organizations o ON o.id = m.org_id
           WHERE m.user_id = $1 AND o.deleted_at IS NULL
           ORDER BY m.created_at ASC, m.org_id ASC
           LIMIT 1"#,
    )
    .bind(user.0)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("oldest_membership_for_user: {e}")))?;
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

    // Per-user advisory lock serialises concurrent owner-org creates for the
    // same user. Without it the count-subquery + INSERT in the membership
    // step races under READ COMMITTED — two transactions can both observe
    // count < limit and both succeed, blowing past the cap. The lock is held
    // for the duration of the transaction and released on commit/rollback.
    advisory_xact_lock(&mut *tx, &user_lock_key(user))
        .await
        .context("create_org_with_owner: advisory lock")?;

    let row: Option<OrgRow> = sqlx::query_as(
        r#"INSERT INTO organizations (slug, name)
           VALUES ($1, $2)
           ON CONFLICT (slug) WHERE deleted_at IS NULL DO NOTHING
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

/// First-org-at-signup creator. Bypasses [`create_org_with_owner`]'s
/// per-user owner-limit because a brand-new account needs at least one
/// org to be usable; the limit only kicks in for additional, user-named
/// orgs created after signup.
///
/// The slug is generated by [`generate_signup_slug`]; on the rare collision
/// the caller retries with a fresh slug.
///
/// Runs inside an existing `Transaction` so the signup tx in the OAuth
/// callback can roll back atomically on failure.
pub async fn create_signup_org_with_owner_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: UserId,
    slug: &str,
    name: &str,
) -> Result<Option<OrgId>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO organizations (slug, name) VALUES ($1, $2) \
         ON CONFLICT (slug) WHERE deleted_at IS NULL DO NOTHING RETURNING id",
    )
    .bind(slug)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await
    .context("create_signup_org_with_owner_in_tx: insert org")?;
    let Some((org_id,)) = row else {
        return Ok(None);
    };
    sqlx::query("INSERT INTO memberships (user_id, org_id, role) VALUES ($1, $2, 'owner')")
        .bind(user.0)
        .bind(org_id)
        .execute(&mut **tx)
        .await
        .context("create_signup_org_with_owner_in_tx: insert membership")?;
    record_audit_tx(
        &mut *tx,
        OrgId(org_id),
        Some(user),
        "org.created",
        Value::Null,
    )
    .await
    .context("create_signup_org_with_owner_in_tx: audit")?;
    Ok(Some(OrgId(org_id)))
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

/// The one membership-count predicate. Shared so the cap enforced inside
/// [`add_member`] and the number [`active_member_count`] reports (which feeds
/// `/usage`) can never drift apart — "enforced == reported" is structural,
/// not comment-enforced.
pub(crate) const MEMBERSHIP_COUNT_SQL: &str = "SELECT count(*) FROM memberships WHERE org_id = $1";

/// Number of members in an org. Same scope as [`list_members`] (memberships
/// are hard-deleted on removal, so no soft-delete filter) — the usage view
/// and the member list never disagree.
pub async fn active_member_count(pool: &PgPool, org: OrgId) -> Result<u32> {
    let (count,): (i64,) = sqlx::query_as(MEMBERSHIP_COUNT_SQL)
        .bind(org.0)
        .fetch_one(pool)
        .await
        .context("active_member_count")?;
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

/// Atomic, partial update of mutable org fields. Either or both of `new_name`
/// / `new_slug` may be `None`; the caller must supply at least one (an
/// all-`None` call is a logic error and rejected with a debug assertion).
///
/// Returns:
///  * [`UpdateOrgOutcome::Updated`] with the post-update row — including the
///    no-op case where the new value(s) equal the current ones. No audit row
///    is written for a no-op so audit history stays meaningful.
///  * [`UpdateOrgOutcome::NotFound`] if the org is missing or soft-deleted.
///  * [`UpdateOrgOutcome::SlugTaken`] when `new_slug` collides with another
///    row (including soft-deleted: slugs stay reserved through the grace
///    window — the unique index doesn't exclude tombstones).
///
/// One transaction covers the row lock, the write, and one audit row per
/// *actually-changed* field, so a combined `{name, slug}` PATCH can't leave
/// the org half-updated. Sub-second-level row lock via `FOR UPDATE` in the
/// CTE serialises concurrent renames of the same org.
pub async fn update_org_fields(
    pool: &PgPool,
    org: OrgId,
    actor: UserId,
    new_name: Option<&str>,
    new_slug: Option<&str>,
) -> Result<UpdateOrgOutcome> {
    debug_assert!(
        new_name.is_some() || new_slug.is_some(),
        "update_org_fields called with no mutable field; handler should reject empty PATCH"
    );

    let mut tx = pool.begin().await.context("update_org_fields: begin")?;

    // One CTE+UPDATE: lock the row, read the prior (slug, name), write only
    // the supplied fields (COALESCE keeps untouched columns), and return both
    // pre- and post- values. `RETURNING` only fires when a row was updated,
    // so missing/soft-deleted orgs yield `None` here → NotFound without a
    // second round-trip.
    let res: std::result::Result<Option<UpdateRow>, sqlx::Error> = sqlx::query_as(
        r#"
        WITH prev AS (
            SELECT id, slug::text AS slug, name FROM organizations
            WHERE id = $1 AND deleted_at IS NULL
            FOR UPDATE
        )
        UPDATE organizations o
           SET name       = COALESCE($2, o.name),
               slug       = COALESCE($3::citext, o.slug),
               updated_at = now()
          FROM prev
         WHERE o.id = prev.id
        RETURNING o.id,
                  o.slug::text AS slug,
                  o.name,
                  o.created_at,
                  o.updated_at,
                  o.deleted_at,
                  prev.slug AS prev_slug,
                  prev.name AS prev_name
        "#,
    )
    .bind(org.0)
    .bind(new_name)
    .bind(new_slug)
    .fetch_optional(&mut *tx)
    .await;
    let row = match res {
        Ok(Some(r)) => r,
        Ok(None) => {
            tx.rollback().await.ok();
            return Ok(UpdateOrgOutcome::NotFound);
        }
        Err(e) if is_unique_violation(&e) => {
            tx.rollback().await.ok();
            return Ok(UpdateOrgOutcome::SlugTaken);
        }
        Err(e) => {
            return Err(AppError::Other(
                anyhow::Error::from(e).context("update_org_fields: update"),
            ));
        }
    };

    let slug_changed = row.slug != row.prev_slug;
    let name_changed = row.name != row.prev_name;
    if slug_changed {
        record_audit_tx(
            &mut tx,
            org,
            Some(actor),
            "org.slug_changed",
            serde_json::json!({ "from": row.prev_slug, "to": row.slug }),
        )
        .await
        .context("update_org_fields: audit slug")?;
    }
    if name_changed {
        record_audit_tx(
            &mut tx,
            org,
            Some(actor),
            "org.renamed",
            serde_json::json!({ "name": row.name }),
        )
        .await
        .context("update_org_fields: audit name")?;
    }
    tx.commit().await.context("update_org_fields: commit")?;
    Ok(UpdateOrgOutcome::Updated(row.into_org()))
}

#[derive(Debug, Clone)]
pub enum UpdateOrgOutcome {
    Updated(Organization),
    NotFound,
    SlugTaken,
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e.as_database_error(), Some(db) if db.code().as_deref() == Some("23505"))
}

#[derive(sqlx::FromRow)]
struct UpdateRow {
    id: Uuid,
    slug: String,
    name: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    prev_slug: String,
    prev_name: String,
}

impl UpdateRow {
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

/// Outcome of [`add_member`]. The invitation accept flow distinguishes
/// "already a member" (no-op, succeed idempotently) from a fresh insert,
/// and from a rejection because the org is at its member cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMemberOutcome {
    Added,
    AlreadyMember,
    /// The org already holds `limit` members; the row was not inserted.
    /// `current` is the member count before this attempt.
    LimitReached {
        current: i64,
        limit: i64,
    },
}

/// Add a user to an org with the given role. Single audit row per successful
/// insert; idempotent — re-adding an existing member is a no-op.
///
/// This is the canonical site for invitation acceptance and any future flow
/// that grants org access. Kept here (not in `auth::invitations`) so the
/// `memberships` table has exactly one writer outside of org create/delete.
///
/// `actor` is the user the audit row should be attributed to. For
/// self-redeem of an invitation pass `user`; for an owner-initiated add
/// (future flow) pass the owner. Mirrors the `remove_member(actor, user)`
/// signature so the audit log records *who* effected the change, not just
/// *who* was changed.
///
/// `max_members` is the plan cap. It is enforced atomically here under a
/// per-org advisory lock (same lock key as `invitations::create`, so an
/// invite and an accept on the same org serialise): the row is inserted,
/// then the post-insert count is taken inside the same transaction and the
/// insert is rolled back if it crossed the cap. Re-adding an existing member
/// stays a no-op and is never rejected by the cap.
pub async fn add_member(
    pool: &PgPool,
    org: OrgId,
    actor: UserId,
    user: UserId,
    role: Role,
    max_members: u32,
) -> Result<AddMemberOutcome> {
    let mut tx = pool.begin().await.context("add_member: begin")?;
    advisory_xact_lock(&mut *tx, &org_lock_key(org))
        .await
        .context("add_member: advisory lock")?;
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        r#"INSERT INTO memberships (user_id, org_id, role)
           VALUES ($1, $2, $3)
           ON CONFLICT (user_id, org_id) DO NOTHING
           RETURNING org_id"#,
    )
    .bind(user.0)
    .bind(org.0)
    .bind(role.as_db_str())
    .fetch_optional(&mut *tx)
    .await
    .context("add_member: insert")?;
    if inserted.is_none() {
        tx.rollback().await.ok();
        return Ok(AddMemberOutcome::AlreadyMember);
    }
    // Count *after* the insert (same predicate as `active_member_count`, so
    // the enforced number equals the number `/usage` reports). If this fresh
    // row crossed the cap, undo it — never let the org exceed `max_members`.
    let (members,): (i64,) = sqlx::query_as(MEMBERSHIP_COUNT_SQL)
        .bind(org.0)
        .fetch_one(&mut *tx)
        .await
        .context("add_member: count members")?;
    let limit = i64::from(max_members);
    if members > limit {
        tx.rollback().await.ok();
        return Ok(AddMemberOutcome::LimitReached {
            current: members - 1,
            limit,
        });
    }
    record_audit_tx(
        &mut tx,
        org,
        Some(actor),
        "member.added",
        serde_json::json!({ "user_id": user.0, "role": role.as_db_str() }),
    )
    .await
    .context("add_member: audit")?;
    tx.commit().await.context("add_member: commit")?;
    Ok(AddMemberOutcome::Added)
}

/// AUTHENTICATED-PATH ONLY. Resolves an org slug to its id for callers that
/// have already proved identity (API token, session). Filters
/// `deleted_at IS NULL` and nothing else — callers must layer additional
/// access control (e.g. [`is_active_member`]) before they trust the result.
///
/// Used by the API-token request path: tokens carry no active-org state, so
/// `X-Uptimepage-Org: <slug>` is the only way the handler learns which
/// org to scope to.
///
/// Do NOT call this from any unauthenticated path. For
/// `*.{base_domain}` lookups use [`find_public_status_org_by_slug`] instead
/// — that helper additionally filters `public_status_enabled = true` and
/// returns a [`PublicStatusOrg`] newtype the operator path can't accept.
/// Substituting the two would expose every org's public surface regardless
/// of opt-in.
pub async fn find_id_by_slug(pool: &PgPool, slug: &str) -> Result<Option<OrgId>> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM organizations WHERE slug = $1 AND deleted_at IS NULL")
            .bind(slug)
            .fetch_optional(pool)
            .await
            .context("find_id_by_slug")?;
    Ok(row.map(|(o,)| OrgId(o)))
}

/// Newtype returned by [`find_public_status_page_by_slug`]. Carries the
/// resolved [`PageRef`] from the unauthenticated public-status path, kept
/// distinct from operator/API-token resolution (opposite security envelopes)
/// so the two can't share lookup helpers.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedPublicPage(pub PageRef);

/// PUBLIC-STATUS PATH ONLY. Resolves a page slug to a [`ResolvedPublicPage`]
/// for the `*.{base_domain}` surface. Filters `status_pages.enabled = true` and
/// the owning org not soft-deleted — if either fails the row is invisible (the
/// handler maps `None` to 404).
pub async fn find_public_status_page_by_slug(
    pool: &PgPool,
    slug: &str,
) -> Result<Option<ResolvedPublicPage>> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        r#"/* SAFE: public-page lookup is intentionally not org_id-scoped */
           SELECT sp.id, sp.org_id
           FROM status_pages sp
           JOIN organizations o ON o.id = sp.org_id
           WHERE sp.slug = $1
             AND sp.enabled = true
             AND o.deleted_at IS NULL"#,
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("find_public_status_page_by_slug")?;
    Ok(row.map(|(page, org)| {
        ResolvedPublicPage(PageRef {
            page: StatusPageId(page),
            org: OrgId(org),
        })
    }))
}

/// PUBLIC-STATUS PATH ONLY (path-based / self-host). The lone live org's PRIMARY
/// page — its oldest page (the one signup created), if enabled. Pinning to the
/// oldest page keeps `/status` stable as other pages are added or toggled:
/// disabling the primary page 404s rather than silently switching the public
/// surface to a younger (e.g. internal) page. 404s (via `None`) if there is no
/// single live org, the org has no page, or the primary page is disabled.
pub async fn resolve_default_page_for_lone_org(pool: &PgPool) -> Result<Option<PageRef>> {
    let Some(org) = find_lone_active_org(pool).await? else {
        return Ok(None);
    };
    let row: Option<(Uuid, bool)> = sqlx::query_as(
        r#"SELECT id, enabled FROM status_pages
           WHERE org_id = $1
           ORDER BY created_at ASC, id ASC
           LIMIT 1"#,
    )
    .bind(org.0)
    .fetch_optional(pool)
    .await
    .context("resolve_default_page_for_lone_org")?;
    Ok(row
        .filter(|(_, enabled)| *enabled)
        .map(|(page, _)| PageRef {
            page: StatusPageId(page),
            org,
        }))
}

/// Org name + page slug + the page's display branding, as needed to render a
/// status page (header falls back to the org name when `public_display_name`
/// is unset) or to build its live URL (`slug`).
#[derive(Debug, Clone)]
pub struct OrgBranding {
    pub name: String,
    pub slug: String,
    pub branding: PublicOrgBranding,
}

impl OrgBranding {
    /// Header name: the page's display override if non-blank, else the org name.
    pub fn resolved_display_name(&self) -> &str {
        self.branding
            .public_display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.name.as_str())
    }
}

/// PUBLIC-STATUS PATH ONLY. Loads a page's display branding (+ the owning org's
/// name for the header fallback) by page id. Keyed by id because the surface
/// that resolved the page already gated visibility; the org-soft-delete filter
/// stays so a tombstoned org renders nothing.
pub async fn load_page_branding(pool: &PgPool, page: StatusPageId) -> Result<Option<OrgBranding>> {
    let row: Option<BrandingRow> = sqlx::query_as(
        r#"SELECT o.name,
                  sp.slug::text AS slug,
                  sp.public_display_name,
                  sp.public_about,
                  sp.public_brand_color,
                  (SELECT pa.content_hash FROM page_assets pa
                   WHERE pa.status_page_id = sp.id AND pa.slot = 'logo') AS logo_hash,
                  sp.public_show_powered_by,
                  sp.public_style
             FROM status_pages sp
             JOIN organizations o ON o.id = sp.org_id
            WHERE sp.id = $1
              AND o.deleted_at IS NULL"#,
    )
    .bind(page.0)
    .fetch_optional(pool)
    .await
    .context("load_page_branding")?;
    Ok(row.map(|r| OrgBranding {
        name: r.name,
        slug: r.slug,
        branding: PublicOrgBranding {
            public_display_name: r.public_display_name,
            public_about: r.public_about,
            public_brand_color: r.public_brand_color,
            logo_hash: r.logo_hash,
            public_show_powered_by: r.public_show_powered_by,
            public_style: PublicStyle::from_db(&r.public_style),
        },
    }))
}

/// Look up a user's id by email (CITEXT). Drives the invitation flow's
/// "is the recipient already a member here?" check before INSERT, and the
/// "already-existing user redeems invitation" path on accept.
pub async fn find_user_by_email(pool: &PgPool, email: &str) -> Result<Option<UserId>> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE email = $1::citext AND deleted_at IS NULL")
            .bind(email)
            .fetch_optional(pool)
            .await
            .context("find_user_by_email")?;
    Ok(row.map(|(u,)| UserId(u)))
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

/// Append an atomic row to the compliance-grade `org_audit_log`.
///
/// This is the **only** writer to `org_audit_log`. The `&mut Transaction`
/// signature is load-bearing: the audit row commits with its data change,
/// so a reader of the log reads exactly the state transitions that
/// actually happened. The downstream contract — GDPR DSR exports, SOC2
/// trails, "who renamed this org" queries — depends on every row being
/// present and atomic with its mutation.
///
/// Do not call from `tokio::spawn`, do not fire-and-forget, do not write
/// to `org_audit_log` from anywhere else. High-volume best-effort events
/// (rate-limit hits, quota blocks) go to `quota_events` via
/// [`crate::quotas::service::record_quota_event`] — a different table
/// with a different durability contract, by design.
///
/// The single-writer invariant is fenced in
/// `scripts/sg-rules/org_audit_single_writer.yml`.
pub(crate) async fn record_audit_tx(
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
struct BrandingRow {
    name: String,
    slug: String,
    public_display_name: Option<String>,
    public_about: Option<String>,
    public_brand_color: Option<String>,
    logo_hash: Option<String>,
    public_show_powered_by: Option<bool>,
    public_style: String,
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
