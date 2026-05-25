//! Sticky last-good cache for `domain_expiry` checks. See
//! `migrations/postgres/014_domain_expiry_state.up.sql` for the row shape and
//! the reasoning behind the design.
//!
//! Every method takes `OrgId` and filters on it. The store row carries
//! `target_id` (PK + FK CASCADE) and a denormalised `org_id`; both must
//! match for any operation to read or mutate the row.
//!
//! Caller responsibility: the `OrgId` passed in MUST be the authenticated
//! tenant for the current request, sourced from session/auth middleware —
//! not a parameter the user controls. `OrgId(Uuid)` is publicly
//! constructible, so the storage layer cannot verify it on its own. The
//! filter exists to make handlers that source `target_id` from request
//! input safe *as long as the OrgId comes from the auth context*; it is
//! not a substitute for the auth context.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::domain::OrgId;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct DomainExpiryState {
    pub domain: String,
    pub expiry_at: DateTime<Utc>,
    pub registrar: Option<String>,
    /// Moment of the last *successful* probe. Never advanced by failures —
    /// the staleness ceiling depends on this.
    pub last_success_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub attempts: i32,
}

#[async_trait]
pub trait DomainExpiryStateStore: Send + Sync {
    async fn get(&self, org: OrgId, target: Uuid) -> Result<Option<DomainExpiryState>>;
    async fn upsert_success(
        &self,
        org: OrgId,
        target: Uuid,
        domain: &str,
        expiry_at: DateTime<Utc>,
        registrar: Option<&str>,
    ) -> Result<()>;
    /// Combined fetch + failure record in one round-trip. Returns the row
    /// AFTER the failure counters are bumped — the caller decides
    /// independently whether to serve the row based on staleness.
    /// Returns `None` for missing rows; the executor treats `None` as
    /// "no last-good, escalate to Error".
    async fn record_failure_returning(
        &self,
        org: OrgId,
        target: Uuid,
        error: &str,
    ) -> Result<Option<DomainExpiryState>>;
}

pub struct PgDomainExpiryStateStore {
    pool: PgPool,
}

impl PgDomainExpiryStateStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DomainExpiryStateStore for PgDomainExpiryStateStore {
    async fn get(&self, org: OrgId, target: Uuid) -> Result<Option<DomainExpiryState>> {
        let row: Option<DomainExpiryStateRow> = sqlx::query_as(
            r#"SELECT domain, expiry_at, registrar, last_success_at,
                      last_attempt_at, last_error, attempts
               FROM domain_expiry_state
               WHERE target_id = $1 AND org_id = $2"#,
        )
        .bind(target)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("get expiry state: {e}")))?;
        Ok(row.map(Into::into))
    }

    async fn upsert_success(
        &self,
        org: OrgId,
        target: Uuid,
        domain: &str,
        expiry_at: DateTime<Utc>,
        registrar: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO domain_expiry_state
                   (target_id, org_id, domain, expiry_at, registrar,
                    last_success_at, last_attempt_at, attempts)
               VALUES ($1, $2, $3, $4, $5, now(), now(), 0)
               ON CONFLICT (target_id) DO UPDATE SET
                   domain = EXCLUDED.domain,
                   expiry_at = EXCLUDED.expiry_at,
                   registrar = EXCLUDED.registrar,
                   last_success_at = EXCLUDED.last_success_at,
                   last_attempt_at = EXCLUDED.last_attempt_at,
                   last_error = NULL,
                   attempts = 0"#,
        )
        .bind(target)
        .bind(org.0)
        .bind(domain)
        .bind(expiry_at)
        .bind(registrar)
        .execute(&self.pool)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("upsert expiry state: {e}")))?;
        Ok(())
    }

    async fn record_failure_returning(
        &self,
        org: OrgId,
        target: Uuid,
        error: &str,
    ) -> Result<Option<DomainExpiryState>> {
        let row: Option<DomainExpiryStateRow> = sqlx::query_as(
            r#"UPDATE domain_expiry_state
               SET last_attempt_at = now(),
                   last_error = $3,
                   attempts = attempts + 1
               WHERE target_id = $1 AND org_id = $2
               RETURNING domain, expiry_at, registrar, last_success_at,
                         last_attempt_at, last_error, attempts"#,
        )
        .bind(target)
        .bind(org.0)
        .bind(error)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            crate::error::AppError::Other(anyhow::anyhow!("record_failure_returning: {e}"))
        })?;
        Ok(row.map(Into::into))
    }
}

#[derive(sqlx::FromRow)]
struct DomainExpiryStateRow {
    domain: String,
    expiry_at: DateTime<Utc>,
    registrar: Option<String>,
    last_success_at: DateTime<Utc>,
    last_attempt_at: DateTime<Utc>,
    last_error: Option<String>,
    attempts: i32,
}

impl From<DomainExpiryStateRow> for DomainExpiryState {
    fn from(r: DomainExpiryStateRow) -> Self {
        Self {
            domain: r.domain,
            expiry_at: r.expiry_at,
            registrar: r.registrar,
            last_success_at: r.last_success_at,
            last_attempt_at: r.last_attempt_at,
            last_error: r.last_error,
            attempts: r.attempts,
        }
    }
}

#[derive(Default)]
pub struct InMemoryDomainExpiryStateStore {
    inner: Mutex<HashMap<(OrgId, Uuid), DomainExpiryState>>,
}

impl InMemoryDomainExpiryStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[doc(hidden)]
    pub fn inner_mut_for_test(
        &self,
    ) -> parking_lot::MutexGuard<'_, HashMap<(OrgId, Uuid), DomainExpiryState>> {
        self.inner.lock()
    }
}

#[async_trait]
impl DomainExpiryStateStore for InMemoryDomainExpiryStateStore {
    async fn get(&self, org: OrgId, target: Uuid) -> Result<Option<DomainExpiryState>> {
        Ok(self.inner.lock().get(&(org, target)).cloned())
    }

    async fn upsert_success(
        &self,
        org: OrgId,
        target: Uuid,
        domain: &str,
        expiry_at: DateTime<Utc>,
        registrar: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now();
        self.inner.lock().insert(
            (org, target),
            DomainExpiryState {
                domain: domain.to_owned(),
                expiry_at,
                registrar: registrar.map(str::to_owned),
                last_success_at: now,
                last_attempt_at: now,
                last_error: None,
                attempts: 0,
            },
        );
        Ok(())
    }

    async fn record_failure_returning(
        &self,
        org: OrgId,
        target: Uuid,
        error: &str,
    ) -> Result<Option<DomainExpiryState>> {
        let mut g = self.inner.lock();
        let Some(state) = g.get_mut(&(org, target)) else {
            return Ok(None);
        };
        state.last_attempt_at = Utc::now();
        state.last_error = Some(error.to_owned());
        state.attempts += 1;
        Ok(Some(state.clone()))
    }
}
