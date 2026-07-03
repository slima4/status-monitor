//! Storage for `heartbeat_monitors`: inbound ping state per heartbeat-kind
//! target. Token discipline mirrors `monitor_shares`: SHA-256 hex is the
//! lookup key, an encrypted copy re-displays the ping URL. The
//! disabled→enabled re-arm lives in `PostgresTargetStore`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::OrgId;
use crate::error::{AppError, Result};
use crate::security::Cipher;
use crate::storage::capability_token;

/// One heartbeat monitor's ping surface. `token` is `None` when the KEK
/// rotated out and the encrypted copy can't be opened.
#[derive(Debug, Clone)]
pub struct HeartbeatMonitor {
    pub token: Option<String>,
    pub last_ping_at: Option<DateTime<Utc>>,
    /// Re-arm point: set at creation and on every disabled→enabled flip.
    pub armed_at: DateTime<Utc>,
}

impl HeartbeatMonitor {
    pub fn anchor(&self) -> DateTime<Utc> {
        anchor_of(self.armed_at, self.last_ping_at)
    }
}

/// The anchor rule (one owner, also used by `AdminRepo`): later of the last
/// real ping and the re-arm point.
pub(crate) fn anchor_of(
    armed_at: DateTime<Utc>,
    last_ping_at: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    last_ping_at.map_or(armed_at, |p| p.max(armed_at))
}

#[async_trait]
pub trait HeartbeatStore: Send + Sync {
    /// Create the row (minting a token) if absent, else return the existing
    /// row. `None` when the target is not in `org`.
    async fn ensure(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>>;
    /// Plain read, never mints, so a read-scoped credential can't create a
    /// write capability.
    async fn get(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>>;
    /// Drop the row (kind switched away). Target deletes cascade via the FK.
    async fn remove(&self, org: OrgId, target_id: Uuid) -> Result<bool>;
    /// Record a ping presented as a raw token, in one statement. `None` for
    /// unknown tokens and soft-deleted orgs, so both 404 the same way.
    async fn record_ping_by_token(&self, raw_token: &str) -> Result<Option<(Uuid, DateTime<Utc>)>>;
}

pub struct PgHeartbeatStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
}

impl PgHeartbeatStore {
    pub fn new(pool: PgPool, cipher: Option<Arc<Cipher>>) -> Self {
        Self { pool, cipher }
    }
}

#[derive(sqlx::FromRow)]
struct HeartbeatRow {
    token_enc: String,
    last_ping_at: Option<DateTime<Utc>>,
    armed_at: DateTime<Utc>,
}

impl HeartbeatRow {
    fn into_monitor(self, cipher: Option<&Cipher>) -> HeartbeatMonitor {
        HeartbeatMonitor {
            token: capability_token::open(&self.token_enc, cipher),
            last_ping_at: self.last_ping_at,
            armed_at: self.armed_at,
        }
    }
}

const HB_COLUMNS: &str = "token_enc, last_ping_at, armed_at";

#[async_trait]
impl HeartbeatStore for PgHeartbeatStore {
    async fn ensure(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        if let Some(existing) = self.get(org, target_id).await? {
            return Ok(Some(existing));
        }
        let minted = capability_token::mint(self.cipher.as_deref())?;
        // SELECT-from-targets scopes the insert to the org; ON CONFLICT keeps a
        // token minted by a concurrent create stable.
        sqlx::query(
            "INSERT INTO heartbeat_monitors (target_id, org_id, token_hash, token_enc) \
             SELECT t.id, t.org_id, $3, $4 FROM targets t \
             WHERE t.id = $2 AND t.org_id = $1 \
             ON CONFLICT (target_id) DO NOTHING",
        )
        .bind(org.0)
        .bind(target_id)
        .bind(&minted.hash)
        .bind(&minted.sealed)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        self.get(org, target_id).await
    }

    async fn get(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        let row: Option<HeartbeatRow> = sqlx::query_as(&format!(
            "SELECT {HB_COLUMNS} FROM heartbeat_monitors WHERE org_id = $1 AND target_id = $2"
        ))
        .bind(org.0)
        .bind(target_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|r| r.into_monitor(self.cipher.as_deref())))
    }

    async fn remove(&self, org: OrgId, target_id: Uuid) -> Result<bool> {
        let res =
            sqlx::query("DELETE FROM heartbeat_monitors WHERE org_id = $1 AND target_id = $2")
                .bind(org.0)
                .bind(target_id)
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn record_ping_by_token(&self, raw_token: &str) -> Result<Option<(Uuid, DateTime<Utc>)>> {
        let row: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            "UPDATE heartbeat_monitors hm SET last_ping_at = now() \
             FROM organizations o \
             WHERE hm.token_hash = $1 AND o.id = hm.org_id AND o.deleted_at IS NULL \
             RETURNING hm.target_id, hm.last_ping_at",
        )
        .bind(capability_token::hash(raw_token))
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row)
    }
}

fn db_err(e: sqlx::Error) -> AppError {
    AppError::Other(anyhow::anyhow!("heartbeat_monitors: {e}"))
}

// ── In-memory store (no-DB harnesses) ─────────────────────────────────────────

struct MemHeartbeat {
    org: OrgId,
    target_id: Uuid,
    monitor: HeartbeatMonitor,
    token_hash: String,
}

/// In-memory [`HeartbeatStore`] for no-DB harnesses. Has no target/org tables,
/// so `ensure` can't verify org membership and `record_ping_by_token` can't
/// drop soft-deleted orgs; DB-backed tests cover those guards.
#[derive(Default)]
pub struct InMemoryHeartbeatStore {
    inner: std::sync::Mutex<Vec<MemHeartbeat>>,
}

impl InMemoryHeartbeatStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl HeartbeatStore for InMemoryHeartbeatStore {
    async fn ensure(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        let mut st = self.inner.lock().unwrap();
        if let Some(m) = st.iter().find(|m| m.org == org && m.target_id == target_id) {
            return Ok(Some(m.monitor.clone()));
        }
        let minted = capability_token::mint(None)?;
        let monitor = HeartbeatMonitor {
            token: Some(minted.raw),
            last_ping_at: None,
            armed_at: Utc::now(),
        };
        st.push(MemHeartbeat {
            org,
            target_id,
            monitor: monitor.clone(),
            token_hash: minted.hash,
        });
        Ok(Some(monitor))
    }

    async fn get(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.org == org && m.target_id == target_id)
            .map(|m| m.monitor.clone()))
    }

    async fn remove(&self, org: OrgId, target_id: Uuid) -> Result<bool> {
        let mut st = self.inner.lock().unwrap();
        let before = st.len();
        st.retain(|m| !(m.org == org && m.target_id == target_id));
        Ok(st.len() < before)
    }

    async fn record_ping_by_token(&self, raw_token: &str) -> Result<Option<(Uuid, DateTime<Utc>)>> {
        let hash = capability_token::hash(raw_token);
        let now = Utc::now();
        Ok(self
            .inner
            .lock()
            .unwrap()
            .iter_mut()
            .find(|m| m.token_hash == hash)
            .map(|m| {
                m.monitor.last_ping_at = Some(now);
                (m.target_id, now)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_is_idempotent_and_ping_round_trips() {
        let store = InMemoryHeartbeatStore::new();
        let org = OrgId(Uuid::new_v4());
        let target = Uuid::new_v4();
        let a = store.ensure(org, target).await.unwrap().unwrap();
        let b = store.ensure(org, target).await.unwrap().unwrap();
        assert_eq!(a.token, b.token, "repeated ensure must keep the token");
        assert!(a.last_ping_at.is_none());

        let (pinged_target, at) = store
            .record_ping_by_token(a.token.as_deref().unwrap())
            .await
            .unwrap()
            .expect("token resolves");
        assert_eq!(pinged_target, target);
        let read = store.get(org, target).await.unwrap().unwrap();
        assert_eq!(read.last_ping_at, Some(at));
        assert!(
            store.record_ping_by_token("nope").await.unwrap().is_none(),
            "unknown tokens record nothing"
        );
    }

    #[tokio::test]
    async fn removed_token_stops_recording() {
        let store = InMemoryHeartbeatStore::new();
        let org = OrgId(Uuid::new_v4());
        let target = Uuid::new_v4();
        let m = store.ensure(org, target).await.unwrap().unwrap();
        assert!(store.remove(org, target).await.unwrap());
        assert!(
            store
                .record_ping_by_token(m.token.as_deref().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn anchor_is_the_later_of_arm_and_ping() {
        let armed = Utc::now();
        let earlier = armed - chrono::Duration::hours(1);
        let later = armed + chrono::Duration::hours(1);
        assert_eq!(anchor_of(armed, None), armed);
        assert_eq!(anchor_of(armed, Some(earlier)), armed);
        assert_eq!(anchor_of(armed, Some(later)), later);
    }
}
