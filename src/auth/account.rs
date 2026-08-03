//! Self-service account deletion + recovery (GDPR right to erasure).
//!
//! Deletion is a soft-delete with a grace window: the user vanishes from the
//! app immediately (every read filters `deleted_at IS NULL`) and the daily
//! purge job (`jobs::purge_deleted`) hard-erases the row once the grace period
//! elapses *and* no live recovery token remains. The recovery token's
//! `expires_at` is the single source of truth for "is the window still open";
//! the purge job and this module both derive the boundary from the one
//! `deleted_at` stamp rather than independently re-counting 30 days.
//!
//! The blocking decision and every write are ONE transaction under a per-user
//! advisory lock plus the per-org lock `orgs::add_member` takes, so a
//! concurrent invite-accept cannot slip a member into an org between the
//! "is this org safe to tombstone?" check and the tombstone write.

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::api::error::codes;
use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};
use crate::storage::locks::{advisory_xact_lock, org_lock_key, user_delete_lock_key};
use crate::storage::orgs::record_audit_tx;

/// One blocking org in an `OWNS_SHARED_ORGS` rejection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BlockingOrg {
    pub id: Uuid,
    pub slug: String,
}

/// Result of a successful deletion request.
#[derive(Debug, Clone)]
pub struct DeletionOutcome {
    pub email: String,
    /// The grace boundary: [`restore_account`] works until this instant, after
    /// which the hard purge runs.
    pub grace_deadline: DateTime<Utc>,
}

/// Soft-delete the caller's account and schedule the hard purge.
///
/// Flow: BEGIN → locks → blocking check → mutations
/// (re-asserting the invariant at write time) → hashed recovery row → COMMIT.
/// The caller sends the confirmation email *after* this returns (post-commit,
/// so a mail failure can't roll back the deletion the user asked for).
pub async fn request_deletion(
    pool: &PgPool,
    user_id: UserId,
    grace_days: u32,
) -> Result<DeletionOutcome> {
    let mut tx = pool.begin().await.context("delete_account: begin")?;

    // Per-user lock: serialises a double-submit of the delete button and
    // pairs with the per-org locks below so membership is frozen across the
    // blocking decision. `user_delete_lock_key` is a deliberately distinct
    // namespace from `user_lock_key` (owner-org / API-token caps), so account
    // deletion never contends with those — see `storage::locks`.
    advisory_xact_lock(&mut *tx, &user_delete_lock_key(user_id))
        .await
        .context("delete_account: user advisory lock")?;

    // Candidate solo-owned orgs: the caller is an owner and the *only* owner.
    // Ordered by id so the per-org locks are always taken in a stable order
    // (deadlock-free against any other multi-org locker). Only the ids are
    // needed here — slug + the other-member verdict are read back *under the
    // locks* so the blocking decision can't race a concurrent invite-accept.
    let candidate_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT o.id
             FROM organizations o
             JOIN memberships m
               ON m.org_id = o.id AND m.user_id = $1 AND m.role = 'owner'
            WHERE o.deleted_at IS NULL
              AND (SELECT count(*) FROM memberships mo
                    WHERE mo.org_id = o.id AND mo.role = 'owner') = 1
            ORDER BY o.id"#,
    )
    .bind(user_id.0)
    .fetch_all(&mut *tx)
    .await
    .context("delete_account: select solo-owned orgs")?;

    for org_id in &candidate_ids {
        // Same `org_lock_key` as `orgs::add_member` / `invitations::create`,
        // so holding it means no member can be added to this org until our
        // transaction ends.
        advisory_xact_lock(&mut *tx, &org_lock_key(OrgId(*org_id)))
            .await
            .context("delete_account: org advisory lock")?;
    }

    // Re-read the candidates *under the locks* in one query, computing the
    // other-member verdict server-side. Any solo-owned org that still has
    // other members must be transferred or deleted first.
    let verdicts: Vec<(Uuid, String, bool)> = sqlx::query_as(
        r#"SELECT o.id, o.slug::text AS slug,
                  EXISTS (SELECT 1 FROM memberships
                           WHERE org_id = o.id AND user_id <> $2) AS has_others
             FROM organizations o
            WHERE o.id = ANY($1)
            ORDER BY o.id"#,
    )
    .bind(&candidate_ids)
    .bind(user_id.0)
    .fetch_all(&mut *tx)
    .await
    .context("delete_account: re-check members under locks")?;

    let blocking: Vec<BlockingOrg> = verdicts
        .iter()
        .filter(|(_, _, has_others)| *has_others)
        .map(|(id, slug, _)| BlockingOrg {
            id: *id,
            slug: slug.clone(),
        })
        .collect();
    if !blocking.is_empty() {
        tx.rollback().await.ok();
        return Err(AppError::unprocessable_details(
            codes::OWNS_SHARED_ORGS,
            "Transfer ownership or delete these organisations before deleting your account.",
            serde_json::json!({ "orgs": blocking }),
        ));
    }

    // All candidates are now safe to tombstone (none has other members).
    let solo_ids: Vec<Uuid> = verdicts.into_iter().map(|(id, _, _)| id).collect();

    // Soft-delete the user. The `deleted_at IS NULL` guard makes a second
    // deletion of an already-soft-deleted account a clean 4xx rather than a
    // silent re-stamp (the one-active recovery-token unique index is the
    // backstop if this guard is ever bypassed).
    let row: Option<(DateTime<Utc>, String)> = sqlx::query_as(
        r#"UPDATE users
              SET deleted_at = now(), updated_at = now()
            WHERE id = $1 AND deleted_at IS NULL
        RETURNING deleted_at, email::text"#,
    )
    .bind(user_id.0)
    .fetch_optional(&mut *tx)
    .await
    .context("delete_account: soft-delete user")?;
    let Some((deleted_at, email)) = row else {
        tx.rollback().await.ok();
        return Err(AppError::conflict(
            codes::ACCOUNT_ALREADY_DELETED,
            "This account is already scheduled for deletion.",
        ));
    };

    // Tombstone each solo-owned org, re-asserting the no-other-members
    // invariant at write time. Zero rows ⇒ someone joined between the check
    // and here ⇒ roll the whole transaction back and report it as blocked.
    for org_id in &solo_ids {
        let updated: Option<(Uuid,)> = sqlx::query_as(
            r#"UPDATE organizations
                  SET deleted_at = now(), updated_at = now()
                WHERE id = $1
                  AND deleted_at IS NULL
                  AND (SELECT count(*) FROM memberships
                        WHERE org_id = $1 AND user_id <> $2) = 0
            RETURNING id"#,
        )
        .bind(org_id)
        .bind(user_id.0)
        .fetch_optional(&mut *tx)
        .await
        .context("delete_account: tombstone solo org")?;
        if updated.is_none() {
            tx.rollback().await.ok();
            return Err(AppError::unprocessable_details(
                codes::OWNS_SHARED_ORGS,
                "Organisation membership changed during deletion; nothing was deleted. \
                 Retry after transferring or deleting shared organisations.",
                serde_json::json!({ "orgs": [{ "id": org_id }] }),
            ));
        }
        // Audit row per tombstoned org — the same surface `org.deleted` /
        // `org.restored` use, so recovery's `user.deletion_recovered` pairs
        // with it and `list_deleted_orgs_deleted_by` keeps working.
        record_audit_tx(
            &mut tx,
            OrgId(*org_id),
            Some(user_id),
            "user.deletion_requested",
            serde_json::json!({ "user_id": user_id.0 }),
        )
        .await
        .context("delete_account: audit")?;
    }

    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id.0)
        .execute(&mut *tx)
        .await
        .context("delete_account: drop sessions")?;
    sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
        .bind(user_id.0)
        .execute(&mut *tx)
        .await
        .context("delete_account: drop api tokens")?;
    sqlx::query(
        "/* SAFE: user-delete drops pending invites across every org they invited from */ \
         DELETE FROM invitations WHERE inviter_id = $1 AND accepted_at IS NULL",
    )
    .bind(user_id.0)
    .execute(&mut *tx)
    .await
    .context("delete_account: drop pending invitations")?;
    // Drop memberships in *shared* orgs only. The owner membership on each
    // tombstoned solo org is intentionally kept: it is how recovery re-grants
    // access to the restored orgs and how the solo set is re-derived without
    // a schema column. The eventual user hard-purge cascades these away, and
    // the org soft-delete reaches its own grace boundary in lockstep (same
    // `deleted_at`), so nothing leaks past the window.
    sqlx::query("DELETE FROM memberships WHERE user_id = $1 AND org_id <> ALL($2)")
        .bind(user_id.0)
        .bind(&solo_ids)
        .execute(&mut *tx)
        .await
        .context("delete_account: drop shared memberships")?;

    // Grace boundary, anchored to the single `deleted_at` stamp so this and the
    // purge job agree by construction rather than re-deriving "30 days". The
    // soft-deleted user can restore (see `restore_account`) within this window;
    // the purge job hard-deletes after it.
    let grace_deadline = deleted_at + chrono::Duration::days(i64::from(grace_days));

    tx.commit().await.context("delete_account: commit")?;

    Ok(DeletionOutcome {
        email,
        grace_deadline,
    })
}

#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub email: String,
    pub orgs: Vec<Uuid>,
}

/// Restore a soft-deleted account. Signing in does NOT do this: undoing an
/// erasure the user deliberately asked for must be its own deliberate act.
///
/// `None` when the account was already active, so a double-submit doesn't mail
/// the user twice.
pub async fn restore_account(pool: &PgPool, user_id: UserId) -> Result<Option<RestoreOutcome>> {
    let mut tx = pool.begin().await.context("restore: begin")?;
    let restored = undelete_in_tx(&mut tx, user_id).await?;
    let Some(outcome) = restored else {
        tx.rollback().await.ok();
        return Ok(None);
    };
    tx.commit().await.context("restore: commit")?;
    Ok(Some(outcome))
}

/// Clear `users.deleted_at`, lift the tombstone on the orgs they solo-own, and
/// audit it. `None` when the row was already active — a benign race, not an
/// error.
async fn undelete_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
) -> Result<Option<RestoreOutcome>> {
    // Serialise against the daily purge (same per-user lock): without it a
    // restore and a same-user purge can interleave on the last grace day, the
    // org cascade wiping tenant data microseconds before this clears
    // `deleted_at` → user restored into an emptied account.
    advisory_xact_lock(&mut **tx, &user_delete_lock_key(user_id))
        .await
        .context("undelete: user advisory lock")?;

    let row: Option<(String,)> = sqlx::query_as(
        "UPDATE users SET deleted_at = NULL, updated_at = now() \
         WHERE id = $1 AND deleted_at IS NOT NULL \
         RETURNING email::text",
    )
    .bind(user_id.0)
    .fetch_optional(&mut **tx)
    .await
    .context("undelete: un-delete user")?;
    let Some((email,)) = row else {
        return Ok(None);
    };

    // Lift the tombstone on every org the user still owns (the solo orgs whose
    // owner membership we deliberately kept at deletion time).
    let restored: Vec<(Uuid,)> = sqlx::query_as(
        r#"UPDATE organizations o
              SET deleted_at = NULL, updated_at = now()
             FROM memberships m
            WHERE m.org_id = o.id
              AND m.user_id = $1
              AND m.role = 'owner'
              AND o.deleted_at IS NOT NULL
        RETURNING o.id"#,
    )
    .bind(user_id.0)
    .fetch_all(&mut **tx)
    .await
    .context("undelete: un-delete solo orgs")?;

    for (org_id,) in &restored {
        record_audit_tx(
            tx,
            OrgId(*org_id),
            Some(user_id),
            "user.deletion_recovered",
            serde_json::json!({ "user_id": user_id.0 }),
        )
        .await
        .context("undelete: audit")?;
    }

    Ok(Some(RestoreOutcome {
        email,
        orgs: restored.into_iter().map(|(id,)| id).collect(),
    }))
}

/// `last_seen_at` is the session-touched activity timestamp (cookie auth,
/// API tokens, magic-link), not just OAuth handshakes — what users read
/// "Last sign-in" as.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AccountFacts {
    pub created_at: DateTime<Utc>,
    pub provider: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

pub async fn account_facts(pool: &PgPool, user_id: UserId) -> Result<Option<AccountFacts>> {
    Ok(sqlx::query_as::<_, AccountFacts>(
        "SELECT u.created_at, u.last_seen_at, oi.provider \
           FROM users u \
           LEFT JOIN LATERAL ( \
                SELECT provider FROM oauth_identities \
                 WHERE user_id = u.id ORDER BY last_login_at DESC LIMIT 1 \
           ) oi ON true \
          WHERE u.id = $1 AND u.deleted_at IS NULL",
    )
    .bind(user_id.0)
    .fetch_optional(pool)
    .await
    .context("account_facts: query")?)
}
