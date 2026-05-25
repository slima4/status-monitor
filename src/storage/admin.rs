//! Cross-tenant data access. The only escape hatch out of the org-scoped
//! repositories. Every constructor records a static `reason` so an audit grep
//! over the codebase enumerates every cross-tenant entry point.
//!
//! Rules (also enforced by `scripts/sg-rules/admin_repo_static_reason.yml`):
//!  * `AdminRepo::new` requires `&'static str` — dynamic strings would defeat
//!    the audit, since a future caller could format an attacker-controlled
//!    value into the reason field.
//!  * Methods on this type intentionally do **not** filter by `org_id`. The
//!    type itself is the audit signal; readers see `AdminRepo` and know the
//!    query crosses tenant boundaries on purpose.
//!  * No method here returns per-org user-facing data. Only aggregates and
//!    background-job inputs (scheduler enumeration, purge queue, etc.).
//!
//! Audit caveat: the grep `rg 'AdminRepo::new\('` enumerates direct call
//! sites only. A `use AdminRepo as Repo` (or trait-impl rename via macros)
//! would dodge it. Code review must reject such renames; the ast-grep rule
//! has the same blind spot.

use anyhow::Context;
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::{OrgId, Target};
use crate::error::Result;
use crate::security::Cipher;
use crate::storage::postgres::{TargetRow, decode_target_row};

/// Source of every enabled target across every organisation, for the
/// scheduler's global registry. Each target is paired with its owning
/// [`OrgId`] so the scheduler→worker→alert path can resolve channels
/// tenant-scoped (a target only ever reaches its own org's channels).
/// Implemented by [`AdminRepo`] (production) and by
/// [`crate::storage::InMemoryTargetStore`] (single-org test fixtures).
#[async_trait]
pub trait EnabledTargetSource: Send + Sync {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>>;
}

#[async_trait]
impl EnabledTargetSource for AdminRepo {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        AdminRepo::list_all_enabled_targets(self).await
    }
}

/// Keyset cursor over `(org_id, target_id)` ascending. `None` means "start
/// from the beginning"; subsequent pages pass the last row's pair to skip
/// past it on the next read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicTargetCursor {
    pub org_id: OrgId,
    pub target_id: Uuid,
}

impl PublicTargetCursor {
    pub fn after(org_id: OrgId, target_id: Uuid) -> Self {
        Self { org_id, target_id }
    }
}

/// Paginated cross-tenant walk over enabled, public-status targets in *live*
/// organisations. Separate from [`EnabledTargetSource`] because the
/// access pattern is fundamentally different: the scheduler builds a single
/// in-memory registry (full snapshot, infrequent), whereas the incident
/// writer streams every 30 s and must keep page-sized memory + bounded SQL
/// load at 10k+ orgs.
#[async_trait]
pub trait PublicStatusTargetSource: Send + Sync {
    /// Next page of `(org_id, target)` strictly after `after`, up to `limit`
    /// rows, ordered by `(org_id, target_id)` ascending. An empty result
    /// signals the walk is done.
    async fn next_public_status_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>>;
}

#[async_trait]
impl PublicStatusTargetSource for AdminRepo {
    async fn next_public_status_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>> {
        AdminRepo::next_public_status_page(self, after, limit).await
    }
}

/// `targets` row plus its `org_id`. Reuses [`TargetRow`] via `#[sqlx(flatten)]`
/// so the column list stays single-sourced with the org-scoped store.
#[derive(sqlx::FromRow)]
struct OrgTargetRow {
    org_id: Uuid,
    #[sqlx(flatten)]
    target: TargetRow,
}

/// Single source for the cross-tenant target column list. Both the
/// scheduler-snapshot and incident-writer-keyset queries return the same
/// `targets` shape that [`decode_target_row`] consumes.
const TARGET_COLUMNS: &str = "t.org_id, t.id, t.name, t.check_spec, t.interval_secs, t.enabled, t.tags, t.alerts, t.group_name, t.owner_user_id, t.public_status, t.public_name, t.public_description, t.public_group, t.public_sort_order, t.created_at, t.updated_at";

pub struct AdminRepo {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
    reason: &'static str,
}

impl AdminRepo {
    /// `reason` is recorded for audit. Use a short, lower-case identifier
    /// pinned to the call site (e.g. `"scheduler_refresh"`,
    /// `"purge_worker"`). The `&'static str` bound prevents dynamic strings
    /// that would defeat a CI grep.
    pub fn new(pool: PgPool, cipher: Option<Arc<Cipher>>, reason: &'static str) -> Self {
        tracing::debug!(reason, "AdminRepo constructed");
        Self {
            pool,
            cipher,
            reason,
        }
    }

    pub fn reason(&self) -> &'static str {
        self.reason
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// All enabled targets across every *live* organisation. The
    /// `organizations.deleted_at IS NULL` filter is load-bearing: a
    /// soft-deleted org has signalled "stop monitoring me" and the 30-day
    /// recovery grace window must not double as a 30-day stream of
    /// post-deletion check writes, alert deliveries, and ClickHouse rows
    /// the cascade purge then has to clear.
    pub async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        let sql = format!(
            "SELECT {TARGET_COLUMNS} \
             FROM targets t JOIN organizations o ON o.id = t.org_id \
             WHERE t.enabled = true AND o.deleted_at IS NULL"
        );
        let rows: Vec<OrgTargetRow> = sqlx::query_as::<_, OrgTargetRow>(&sql)
            .fetch_all(&self.pool)
            .await
            .context("admin: list all enabled targets")?;
        rows.into_iter()
            .map(|r| {
                decode_target_row(r.target, self.cipher.as_deref()).map(|t| (OrgId(r.org_id), t))
            })
            .collect()
    }

    /// Keyset-paginated walk over enabled, public-status targets in live
    /// orgs. Backed by the partial index `idx_targets_public_page_cursor
    /// (org_id, id) WHERE enabled AND public_status` so per-page cost stays
    /// `O(page_size)` index reads, independent of total target count.
    /// `limit` is clamped to a safety ceiling.
    pub async fn next_public_status_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>> {
        const MAX_PAGE: usize = 2_048;
        let limit = limit.clamp(1, MAX_PAGE) as i64;
        let base = format!(
            "SELECT {TARGET_COLUMNS} \
             FROM targets t JOIN organizations o ON o.id = t.org_id \
             WHERE t.enabled = true AND t.public_status = true AND o.deleted_at IS NULL"
        );
        let rows: Vec<OrgTargetRow> = match after {
            None => {
                let sql = format!("{base} ORDER BY t.org_id, t.id LIMIT $1");
                sqlx::query_as::<_, OrgTargetRow>(&sql)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await
            }
            Some(cursor) => {
                let sql = format!(
                    "{base} AND (t.org_id, t.id) > ($1, $2) ORDER BY t.org_id, t.id LIMIT $3"
                );
                sqlx::query_as::<_, OrgTargetRow>(&sql)
                    .bind(cursor.org_id.0)
                    .bind(cursor.target_id)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .context("admin: next public-status page")?;
        rows.into_iter()
            .map(|r| {
                decode_target_row(r.target, self.cipher.as_deref()).map(|t| (OrgId(r.org_id), t))
            })
            .collect()
    }
}
