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

use crate::domain::Target;
use crate::error::Result;
use crate::security::Cipher;
use crate::storage::postgres::{TargetRow, decode_target_row};

/// Source of every enabled target across every organisation, for the
/// scheduler's global registry. Implemented by [`AdminRepo`] (production) and
/// by [`crate::storage::InMemoryTargetStore`] (single-org test fixtures).
#[async_trait]
pub trait EnabledTargetSource: Send + Sync {
    async fn list_all_enabled_targets(&self) -> Result<Vec<Target>>;
}

#[async_trait]
impl EnabledTargetSource for AdminRepo {
    async fn list_all_enabled_targets(&self) -> Result<Vec<Target>> {
        AdminRepo::list_all_enabled_targets(self).await
    }
}

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

    /// All enabled targets across every organisation. Used by the scheduler
    /// to materialise its global registry — checks must run for tenants the
    /// caller has no session for.
    pub async fn list_all_enabled_targets(&self) -> Result<Vec<Target>> {
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
               FROM targets
               WHERE enabled = true"#,
        )
        .fetch_all(&self.pool)
        .await
        .context("admin: list all enabled targets")?;
        rows.into_iter()
            .map(|r| decode_target_row(r, self.cipher.as_deref()))
            .collect()
    }
}
