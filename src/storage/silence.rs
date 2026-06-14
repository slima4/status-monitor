//! Silence (no-data) detection state.
//!
//! A monitor is *silent* when every region it is assigned to (`target_regions`)
//! has lost its probe — no enabled `agents` row for that region with a fresh
//! `last_seen_at`. Detection is derived from agent liveness, not check results:
//! a failing or timing-out check still reports an `Error` and opens a normal
//! incident, so true silence means the probe stopped running (overwhelmingly,
//! the agent/region died). `agents.last_seen_at` is real liveness — it is bumped
//! on the agent's authenticated config pull, independent of whether any check
//! produced a result.
//!
//! This store runs that detection query and records the open/resolved boundary
//! in `monitor_silence_state` so a silence is announced once and a recovery
//! once. Healthy monitors never get a row.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::OrgId;
use crate::error::Result;

/// An open (unresolved) silence row. `notified` = the customer was already told.
#[derive(Debug, Clone, Copy)]
pub struct OpenSilence {
    pub org: OrgId,
    pub target_id: Uuid,
    pub notified: bool,
}

#[async_trait]
pub trait SilenceStore: Send + Sync {
    /// Enabled, assigned targets whose every region lacks a fresh, enabled agent
    /// (`last_seen_at` within `stale_after_secs`) and that are not in an active
    /// maintenance window. The unmonitored set.
    async fn unmonitored(&self, stale_after_secs: u64) -> Result<Vec<(OrgId, Uuid)>>;
    /// Open silences (`resolved_at IS NULL`), with whether each was notified.
    async fn list_open(&self) -> Result<Vec<OpenSilence>>;
    /// Target ids currently silent for one org — for the grey "no data" overlay
    /// on the dashboard and status page.
    async fn open_target_ids(&self, org: OrgId) -> Result<Vec<Uuid>>;
    /// Region ids with a fresh, enabled agent (probe alive). Powers the
    /// per-region "no data" mark on the monitor detail breakdown table.
    async fn live_regions(&self, stale_after_secs: u64) -> Result<Vec<String>>;
    /// Total enabled targets in live orgs — denominator for mass-outage damping.
    async fn enabled_target_count(&self) -> Result<i64>;
    /// Record a target as silent. Idempotent: an already-open silence keeps its
    /// `silent_since`; re-entering a previously-resolved one starts a fresh
    /// episode (new `silent_since`, cleared `notified_at`/`resolved_at`).
    async fn enter(&self, org: OrgId, target_id: Uuid, at: DateTime<Utc>) -> Result<()>;
    /// Stamp the customer-notified time on an open silence. Idempotent.
    async fn mark_notified(&self, org: OrgId, target_id: Uuid, at: DateTime<Utc>) -> Result<()>;
    /// Mark an open silence resolved (monitoring resumed). Returns whether this
    /// call flipped it — so only the winner of a 2-instance race notifies.
    async fn resolve(&self, org: OrgId, target_id: Uuid, at: DateTime<Utc>) -> Result<bool>;
}

// ── PostgreSQL implementation ────────────────────────────────────────────────

pub struct PgSilenceStore {
    pool: PgPool,
}

impl PgSilenceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SilenceStore for PgSilenceStore {
    async fn unmonitored(&self, stale_after_secs: u64) -> Result<Vec<(OrgId, Uuid)>> {
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            r#"SELECT t.org_id, t.id
               FROM targets t
               JOIN organizations o ON o.id = t.org_id
               WHERE t.enabled AND o.deleted_at IS NULL
                 AND EXISTS (SELECT 1 FROM target_regions tr WHERE tr.target_id = t.id)
                 AND NOT EXISTS (
                     SELECT 1 FROM target_regions tr
                     JOIN regions rg ON rg.id = tr.region AND rg.enabled
                     JOIN agents a   ON a.region = tr.region AND a.enabled
                                    AND a.last_seen_at > now() - ($1::bigint * interval '1 second')
                     WHERE tr.target_id = t.id
                 )
                 AND NOT EXISTS (
                     SELECT 1 FROM maintenance_window_components mwc
                     JOIN maintenance_windows mw ON mw.id = mwc.maintenance_id
                     WHERE mwc.target_id = t.id AND now() BETWEEN mw.starts_at AND mw.ends_at
                 )"#,
        )
        .bind(stale_after_secs as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence unmonitored: {e}"))?;
        Ok(rows.into_iter().map(|(o, t)| (OrgId(o), t)).collect())
    }

    async fn list_open(&self) -> Result<Vec<OpenSilence>> {
        let rows: Vec<(Uuid, Uuid, bool)> = sqlx::query_as(
            "SELECT org_id, target_id, notified_at IS NOT NULL \
             FROM monitor_silence_state WHERE resolved_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence list_open: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|(o, t, notified)| OpenSilence {
                org: OrgId(o),
                target_id: t,
                notified,
            })
            .collect())
    }

    async fn open_target_ids(&self, org: OrgId) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT target_id FROM monitor_silence_state \
             WHERE org_id = $1 AND resolved_at IS NULL",
        )
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence open_target_ids: {e}"))?;
        Ok(rows.into_iter().map(|(t,)| t).collect())
    }

    async fn live_regions(&self, stale_after_secs: u64) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"SELECT DISTINCT a.region
               FROM agents a
               JOIN regions rg ON rg.id = a.region AND rg.enabled
               WHERE a.enabled
                 AND a.last_seen_at > now() - ($1::bigint * interval '1 second')"#,
        )
        .bind(stale_after_secs as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence live_regions: {e}"))?;
        Ok(rows.into_iter().map(|(r,)| r).collect())
    }

    async fn enabled_target_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM targets t \
             JOIN organizations o ON o.id = t.org_id \
             WHERE t.enabled AND o.deleted_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence enabled_target_count: {e}"))?;
        Ok(n)
    }

    async fn enter(&self, org: OrgId, target_id: Uuid, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO monitor_silence_state (target_id, org_id, silent_since)
               VALUES ($1, $2, $3)
               ON CONFLICT (target_id) DO UPDATE SET
                   silent_since = CASE WHEN monitor_silence_state.resolved_at IS NOT NULL
                                       THEN EXCLUDED.silent_since
                                       ELSE monitor_silence_state.silent_since END,
                   notified_at  = CASE WHEN monitor_silence_state.resolved_at IS NOT NULL
                                       THEN NULL
                                       ELSE monitor_silence_state.notified_at END,
                   resolved_at  = NULL"#,
        )
        .bind(target_id)
        .bind(org.0)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence enter: {e}"))?;
        Ok(())
    }

    async fn mark_notified(&self, org: OrgId, target_id: Uuid, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "UPDATE monitor_silence_state SET notified_at = $3 \
             WHERE target_id = $1 AND org_id = $2 AND resolved_at IS NULL AND notified_at IS NULL",
        )
        .bind(target_id)
        .bind(org.0)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence mark_notified: {e}"))?;
        Ok(())
    }

    async fn resolve(&self, org: OrgId, target_id: Uuid, at: DateTime<Utc>) -> Result<bool> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "UPDATE monitor_silence_state SET resolved_at = $3 \
             WHERE target_id = $1 AND org_id = $2 AND resolved_at IS NULL \
             RETURNING target_id",
        )
        .bind(target_id)
        .bind(org.0)
        .bind(at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("silence resolve: {e}"))?;
        Ok(row.is_some())
    }
}

// ── In-memory implementation (for sweep-logic tests) ─────────────────────────

/// Test double. `unmonitored` is a settable fixture (the real query needs PG);
/// `enter`/`resolve`/`list_open` exercise the sweep's diff logic.
#[derive(Default)]
pub struct InMemorySilenceStore {
    inner: parking_lot::Mutex<InMemorySilenceState>,
}

#[derive(Default)]
struct InMemorySilenceState {
    unmonitored: Vec<(OrgId, Uuid)>,
    total: i64,
    open: std::collections::HashMap<Uuid, (OrgId, bool)>,
    resolved: Vec<(OrgId, Uuid)>,
}

impl InMemorySilenceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_unmonitored(&self, set: Vec<(OrgId, Uuid)>) {
        self.inner.lock().unmonitored = set;
    }

    pub fn set_total(&self, total: i64) {
        self.inner.lock().total = total;
    }

    pub fn open_target_ids(&self) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self.inner.lock().open.keys().copied().collect();
        v.sort();
        v
    }

    pub fn resolved_target_ids(&self) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self.inner.lock().resolved.iter().map(|(_, t)| *t).collect();
        v.sort();
        v
    }
}

#[async_trait]
impl SilenceStore for InMemorySilenceStore {
    async fn unmonitored(&self, _stale_after_secs: u64) -> Result<Vec<(OrgId, Uuid)>> {
        Ok(self.inner.lock().unmonitored.clone())
    }

    async fn list_open(&self) -> Result<Vec<OpenSilence>> {
        Ok(self
            .inner
            .lock()
            .open
            .iter()
            .map(|(t, (o, notified))| OpenSilence {
                org: *o,
                target_id: *t,
                notified: *notified,
            })
            .collect())
    }

    async fn open_target_ids(&self, org: OrgId) -> Result<Vec<Uuid>> {
        Ok(self
            .inner
            .lock()
            .open
            .iter()
            .filter(|(_, (o, _))| *o == org)
            .map(|(t, _)| *t)
            .collect())
    }

    async fn live_regions(&self, _stale_after_secs: u64) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn enabled_target_count(&self) -> Result<i64> {
        Ok(self.inner.lock().total)
    }

    async fn enter(&self, org: OrgId, target_id: Uuid, _at: DateTime<Utc>) -> Result<()> {
        self.inner
            .lock()
            .open
            .entry(target_id)
            .or_insert((org, false));
        Ok(())
    }

    async fn mark_notified(&self, _org: OrgId, target_id: Uuid, _at: DateTime<Utc>) -> Result<()> {
        if let Some(v) = self.inner.lock().open.get_mut(&target_id) {
            v.1 = true;
        }
        Ok(())
    }

    async fn resolve(&self, org: OrgId, target_id: Uuid, _at: DateTime<Utc>) -> Result<bool> {
        let mut g = self.inner.lock();
        if g.open.remove(&target_id).is_some() {
            g.resolved.push((org, target_id));
            return Ok(true);
        }
        Ok(false)
    }
}
