//! Postgres transaction-scoped advisory locks.
//!
//! Several flows guard a "count then write under a cap" sequence that is NOT
//! race-safe under READ COMMITTED on its own: concurrent actors each see a
//! snapshot count, all pass `+1 <= limit`, and the cap is overshot. The fix
//! everywhere is the same — take a `pg_advisory_xact_lock` keyed on the
//! subject, then count, then write, all in one transaction.
//!
//! The lock key is load-bearing: flows that must serialise against each other
//! have to hash the *same* string, and flows that must not collide have to
//! differ. That correspondence used to live only in copied SQL string
//! literals and prose comments — one transposed key and the guarantee breaks
//! silently. This module is the single owner of both the lock statement and
//! the key derivation, so the relationships live in one place and are
//! reviewed together rather than re-derived at each call site:
//!
//! - [`org_lock_key`] — one org. Shared by org-member adds, invitation
//!   creation, target create / bulk-create caps, and account deletion's
//!   per-org freeze. All of these must serialise on the same org.
//! - [`user_lock_key`] — one user. Shared by owner-org-create and API-token
//!   caps (per-user count guards).
//! - [`user_delete_lock_key`] — one user, a deliberately distinct namespace
//!   from [`user_lock_key`] so account deletion does not serialise against
//!   unrelated per-user cap writes.

use sqlx::PgExecutor;

use crate::domain::{OrgId, UserId};

/// Lock key for a per-org critical section (membership / target caps, the
/// deletion-time membership freeze).
pub fn org_lock_key(org: OrgId) -> String {
    org.0.to_string()
}

/// Lock key for a per-user cap critical section (owner-org / API-token
/// counts).
pub fn user_lock_key(user: UserId) -> String {
    user.0.to_string()
}

/// Lock key for account deletion. Distinct namespace from [`user_lock_key`]
/// on purpose: deletion must not contend with unrelated per-user cap writes.
pub fn user_delete_lock_key(user: UserId) -> String {
    format!("user_delete:{}", user.0)
}

/// Take a transaction-scoped advisory lock on `key`. Held until the
/// transaction commits or rolls back. Pass the transaction handle
/// (`&mut *tx`); the lock is released automatically with that transaction.
pub async fn advisory_xact_lock<'c, E>(executor: E, key: &str) -> sqlx::Result<()>
where
    E: PgExecutor<'c>,
{
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(key)
        .execute(executor)
        .await
        .map(|_| ())
}
