//! Sticky last-good cache for `domain_expiry` checks. See
//! `migrations/postgres/014_domain_expiry_state.up.sql` for the row shape and
//! the reasoning behind the design.
//!
//! **Invariant — do not break.** This store is keyed by `target_id` (PK +
//! FK CASCADE to `targets`) with no `org_id` column. That's only safe
//! because the sole writer is the worker, which sources `target_id` from
//! `ScheduledTarget` (scheduler-loaded, server-generated). DO NOT wire this
//! store to an HTTP handler that takes `target_id` from request input — add
//! an `OrgId` parameter and gate on it (mirroring `PgIncidentStore::get`)
//! before exposing it across the API boundary.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct DomainExpiryState {
    pub domain: String,
    pub expiry_at: DateTime<Utc>,
    pub registrar: Option<String>,
    pub verified_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub attempts: i32,
}

#[async_trait]
pub trait DomainExpiryStateStore: Send + Sync {
    async fn get(&self, target: Uuid) -> Result<Option<DomainExpiryState>>;
    async fn upsert_success(
        &self,
        target: Uuid,
        domain: &str,
        expiry_at: DateTime<Utc>,
        registrar: Option<&str>,
    ) -> Result<()>;
    /// UPDATE-only: bumps `attempts` / `last_attempt_at` / `last_error` on an
    /// existing row, no-ops when no row exists yet (no successful probe has
    /// landed for this target). Without a prior success there is nothing to
    /// fall back to, so the executor surfaces the raw error instead.
    async fn record_failure(&self, target: Uuid, error: &str) -> Result<()>;
    /// Combined fetch + failure record in one round-trip. Returns the row
    /// AFTER the failure counters are bumped — the caller decides
    /// independently whether to serve the row based on staleness.
    async fn record_failure_returning(
        &self,
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
    async fn get(&self, target: Uuid) -> Result<Option<DomainExpiryState>> {
        let row: Option<DomainExpiryStateRow> = sqlx::query_as(
            r#"SELECT domain, expiry_at, registrar, verified_at,
                      last_attempt_at, last_error, attempts
               FROM domain_expiry_state
               WHERE target_id = $1"#,
        )
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("get expiry state: {e}")))?;
        Ok(row.map(Into::into))
    }

    async fn upsert_success(
        &self,
        target: Uuid,
        domain: &str,
        expiry_at: DateTime<Utc>,
        registrar: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO domain_expiry_state
                   (target_id, domain, expiry_at, registrar,
                    verified_at, last_attempt_at, attempts)
               VALUES ($1, $2, $3, $4, now(), now(), 0)
               ON CONFLICT (target_id) DO UPDATE SET
                   domain = EXCLUDED.domain,
                   expiry_at = EXCLUDED.expiry_at,
                   registrar = EXCLUDED.registrar,
                   verified_at = EXCLUDED.verified_at,
                   last_attempt_at = EXCLUDED.last_attempt_at,
                   last_error = NULL,
                   attempts = 0"#,
        )
        .bind(target)
        .bind(domain)
        .bind(expiry_at)
        .bind(registrar)
        .execute(&self.pool)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("upsert expiry state: {e}")))?;
        Ok(())
    }

    async fn record_failure(&self, target: Uuid, error: &str) -> Result<()> {
        sqlx::query(
            r#"UPDATE domain_expiry_state
               SET last_attempt_at = now(),
                   last_error = $2,
                   attempts = attempts + 1
               WHERE target_id = $1"#,
        )
        .bind(target)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            crate::error::AppError::Other(anyhow::anyhow!("record expiry failure: {e}"))
        })?;
        Ok(())
    }

    async fn record_failure_returning(
        &self,
        target: Uuid,
        error: &str,
    ) -> Result<Option<DomainExpiryState>> {
        let row: Option<DomainExpiryStateRow> = sqlx::query_as(
            r#"UPDATE domain_expiry_state
               SET last_attempt_at = now(),
                   last_error = $2,
                   attempts = attempts + 1
               WHERE target_id = $1
               RETURNING domain, expiry_at, registrar, verified_at,
                         last_attempt_at, last_error, attempts"#,
        )
        .bind(target)
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
    verified_at: DateTime<Utc>,
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
            verified_at: r.verified_at,
            last_attempt_at: r.last_attempt_at,
            last_error: r.last_error,
            attempts: r.attempts,
        }
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryDomainExpiryStateStore {
    inner: Mutex<HashMap<Uuid, DomainExpiryState>>,
}

impl InMemoryDomainExpiryStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[doc(hidden)]
    pub fn inner_mut_for_test(
        &self,
    ) -> parking_lot::MutexGuard<'_, HashMap<Uuid, DomainExpiryState>> {
        self.inner.lock()
    }
}

#[async_trait]
impl DomainExpiryStateStore for InMemoryDomainExpiryStateStore {
    async fn get(&self, target: Uuid) -> Result<Option<DomainExpiryState>> {
        Ok(self.inner.lock().get(&target).cloned())
    }

    async fn upsert_success(
        &self,
        target: Uuid,
        domain: &str,
        expiry_at: DateTime<Utc>,
        registrar: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now();
        self.inner.lock().insert(
            target,
            DomainExpiryState {
                domain: domain.to_owned(),
                expiry_at,
                registrar: registrar.map(str::to_owned),
                verified_at: now,
                last_attempt_at: now,
                last_error: None,
                attempts: 0,
            },
        );
        Ok(())
    }

    async fn record_failure(&self, target: Uuid, error: &str) -> Result<()> {
        if let Some(state) = self.inner.lock().get_mut(&target) {
            state.last_attempt_at = Utc::now();
            state.last_error = Some(error.to_owned());
            state.attempts += 1;
        }
        Ok(())
    }

    async fn record_failure_returning(
        &self,
        target: Uuid,
        error: &str,
    ) -> Result<Option<DomainExpiryState>> {
        let mut g = self.inner.lock();
        let Some(state) = g.get_mut(&target) else {
            return Ok(None);
        };
        state.last_attempt_at = Utc::now();
        state.last_error = Some(error.to_owned());
        state.attempts += 1;
        Ok(Some(state.clone()))
    }
}
