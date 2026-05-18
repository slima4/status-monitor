//! Storage for per-org `notification_channels`.
//!
//! Org scoping is implicit (`default_org_id`), mirroring [`super::maintenance`].
//! The transport secrets in `config` are sealed at rest by the credentials
//! KEK — the same `{"$enc":"v1:…"}` envelope convention used for
//! `targets.check_spec` (see [`super::postgres_secrets`]) — and opened back to
//! a plaintext [`ChannelConfig`] at the DB edge so callers never see ciphertext.

use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{
    ChannelConfig, NewNotificationChannel, NotificationChannel, NotificationChannelUpdate, OrgId,
};
use crate::error::{AppError, Result};
use crate::security::{Cipher, envelope_str, wrap_envelope};
use crate::storage::locks::{advisory_xact_lock, org_lock_key};

/// Serialize + (optionally) seal a config for storage. With no KEK the JSON is
/// stored as-is (no-KEK self-host); with a KEK the whole blob becomes
/// `{"$enc":"v1:…"}`.
fn seal(cfg: &ChannelConfig, cipher: Option<&Cipher>) -> Result<Value> {
    let plain = serde_json::to_value(cfg)
        .map_err(|e| AppError::Other(anyhow!("encode channel config: {e}")))?;
    match cipher {
        None => Ok(plain),
        Some(c) => {
            let bytes = serde_json::to_vec(&plain)
                .map_err(|e| AppError::Other(anyhow!("encode channel config: {e}")))?;
            let env = c
                .encrypt(&bytes)
                .map_err(|e| AppError::Other(anyhow!("seal channel config: {e}")))?;
            Ok(wrap_envelope(env))
        }
    }
}

/// Inverse of [`seal`]. A sealed value with no KEK in scope is a loud error,
/// never a silent plaintext fallthrough.
fn open(value: Value, cipher: Option<&Cipher>) -> Result<ChannelConfig> {
    if let Some(env) = envelope_str(&value) {
        let c = cipher.ok_or_else(|| {
            AppError::Other(anyhow!(
                "notification channel is sealed but no credentials KEK is configured"
            ))
        })?;
        let bytes = c
            .decrypt(env)
            .map_err(|e| AppError::Other(anyhow!("open channel config: {e}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Other(anyhow!("decode channel config: {e}")))
    } else {
        serde_json::from_value(value)
            .map_err(|e| AppError::Other(anyhow!("decode channel config: {e}")))
    }
}

#[async_trait]
pub trait NotificationChannelStore: Send + Sync {
    /// Atomically capped at `max_channels` for the org. A breach returns
    /// `CHANNEL_QUOTA_EXCEEDED`; a duplicate name `CHANNEL_NAME_TAKEN`.
    async fn create(
        &self,
        new: NewNotificationChannel,
        max_channels: i64,
    ) -> Result<NotificationChannel>;
    async fn list(&self) -> Result<Vec<NotificationChannel>>;
    async fn count(&self) -> Result<u64>;
    async fn get(&self, id: Uuid) -> Result<Option<NotificationChannel>>;
    async fn update(
        &self,
        id: Uuid,
        update: NotificationChannelUpdate,
    ) -> Result<Option<NotificationChannel>>;
    async fn delete(&self, id: Uuid) -> Result<bool>;
    /// Subset of `ids` that exist in this org. Mirrors
    /// [`crate::storage::MaintenanceStore::existing_target_ids`] so the
    /// "ids belong to the caller's org" idiom is uniform — used to validate
    /// target alert bindings in one query instead of N point lookups.
    async fn existing_channel_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

/// Org-scoped Postgres store. Every query binds `default_org_id` so one
/// tenant can never read or mutate another's channels.
pub struct PgNotificationChannelStore {
    pool: PgPool,
    default_org_id: OrgId,
    cipher: Option<Arc<Cipher>>,
}

impl PgNotificationChannelStore {
    pub fn new(pool: PgPool, default_org_id: OrgId, cipher: Option<Arc<Cipher>>) -> Self {
        Self {
            pool,
            default_org_id,
            cipher,
        }
    }

    fn org_id(&self) -> Uuid {
        self.default_org_id.0
    }
}

#[derive(sqlx::FromRow)]
struct ChannelRow {
    id: Uuid,
    name: String,
    config: Value,
    enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ChannelRow {
    fn into_channel(self, cipher: Option<&Cipher>) -> Result<NotificationChannel> {
        let config = open(self.config, cipher)?;
        Ok(NotificationChannel {
            id: self.id,
            name: self.name,
            // The decrypted config is the single source of truth for kind;
            // the `kind` text column exists only for indexing/debugging.
            kind: config.kind(),
            config,
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|d| d.is_unique_violation())
}

#[async_trait]
impl NotificationChannelStore for PgNotificationChannelStore {
    async fn create(
        &self,
        new: NewNotificationChannel,
        max_channels: i64,
    ) -> Result<NotificationChannel> {
        let sealed = seal(&new.config, self.cipher.as_deref())?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppError::Other(anyhow!("begin: {e}")))?;
        // The count-subquery + INSERT is not race-safe on its own under READ
        // COMMITTED — two creates at count == cap-1 each see `< cap` and both
        // insert, overshooting the cap. A per-org advisory lock (the same key
        // every other org-cap writer uses) serialises them; it is held until
        // this transaction commits or rolls back.
        advisory_xact_lock(&mut *tx, &org_lock_key(self.default_org_id))
            .await
            .map_err(|e| AppError::Other(anyhow!("advisory lock: {e}")))?;
        let row: Option<ChannelRow> = sqlx::query_as(
            r#"INSERT INTO notification_channels (org_id, name, kind, config, enabled)
               SELECT $1, $2, $3, $4, $5
               WHERE (SELECT count(*) FROM notification_channels WHERE org_id = $1) < $6
               RETURNING id, name, config, enabled, created_at, updated_at"#,
        )
        .bind(self.org_id())
        .bind(&new.name)
        .bind(new.config.kind().as_str())
        .bind(&sealed)
        .bind(new.enabled)
        .bind(max_channels)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::unprocessable(
                    "CHANNEL_NAME_TAKEN",
                    "a notification channel with this name already exists",
                )
            } else {
                AppError::Other(anyhow!("insert notification channel: {e}"))
            }
        })?;
        let Some(row) = row else {
            tx.rollback().await.ok();
            return Err(AppError::unprocessable(
                "CHANNEL_QUOTA_EXCEEDED",
                "notification channel limit reached for this plan",
            ));
        };
        tx.commit()
            .await
            .map_err(|e| AppError::Other(anyhow!("commit: {e}")))?;
        row.into_channel(self.cipher.as_deref())
    }

    async fn list(&self) -> Result<Vec<NotificationChannel>> {
        let rows: Vec<ChannelRow> = sqlx::query_as(
            r#"SELECT id, name, config, enabled, created_at, updated_at
               FROM notification_channels
               WHERE org_id = $1
               ORDER BY name"#,
        )
        .bind(self.org_id())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("list notification channels: {e}")))?;
        rows.into_iter()
            .map(|r| r.into_channel(self.cipher.as_deref()))
            .collect()
    }

    async fn count(&self) -> Result<u64> {
        let (total,): (i64,) =
            sqlx::query_as(r#"SELECT count(*) FROM notification_channels WHERE org_id = $1"#)
                .bind(self.org_id())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| AppError::Other(anyhow!("count notification channels: {e}")))?;
        Ok(total.max(0) as u64)
    }

    async fn get(&self, id: Uuid) -> Result<Option<NotificationChannel>> {
        let row: Option<ChannelRow> = sqlx::query_as(
            r#"SELECT id, name, config, enabled, created_at, updated_at
               FROM notification_channels WHERE id = $1 AND org_id = $2"#,
        )
        .bind(id)
        .bind(self.org_id())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("get notification channel: {e}")))?;
        row.map(|r| r.into_channel(self.cipher.as_deref()))
            .transpose()
    }

    async fn update(
        &self,
        id: Uuid,
        update: NotificationChannelUpdate,
    ) -> Result<Option<NotificationChannel>> {
        // Re-seal only when the config is being changed; `kind` follows it.
        let (sealed, kind) = match &update.config {
            Some(c) => (
                Some(seal(c, self.cipher.as_deref())?),
                Some(c.kind().as_str()),
            ),
            None => (None, None),
        };
        let row: Option<ChannelRow> = sqlx::query_as(
            r#"UPDATE notification_channels
               SET name       = COALESCE($2, name),
                   kind       = COALESCE($3, kind),
                   config     = COALESCE($4, config),
                   enabled    = COALESCE($5, enabled),
                   updated_at = now()
               WHERE id = $1 AND org_id = $6
               RETURNING id, name, config, enabled, created_at, updated_at"#,
        )
        .bind(id)
        .bind(update.name.as_ref())
        .bind(kind)
        .bind(sealed)
        .bind(update.enabled)
        .bind(self.org_id())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::unprocessable(
                    "CHANNEL_NAME_TAKEN",
                    "a notification channel with this name already exists",
                )
            } else {
                AppError::Other(anyhow!("update notification channel: {e}"))
            }
        })?;
        row.map(|r| r.into_channel(self.cipher.as_deref()))
            .transpose()
    }

    async fn delete(&self, id: Uuid) -> Result<bool> {
        let result =
            sqlx::query(r#"DELETE FROM notification_channels WHERE id = $1 AND org_id = $2"#)
                .bind(id)
                .bind(self.org_id())
                .execute(&self.pool)
                .await
                .map_err(|e| AppError::Other(anyhow!("delete notification channel: {e}")))?;
        Ok(result.rows_affected() > 0)
    }

    async fn existing_channel_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"SELECT id FROM notification_channels
               WHERE id = ANY($1::uuid[]) AND org_id = $2"#,
        )
        .bind(ids)
        .bind(self.org_id())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("existing_channel_ids: {e}")))?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryNotificationChannelStore {
    inner: Mutex<Vec<NotificationChannel>>,
}

impl InMemoryNotificationChannelStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

#[async_trait]
impl NotificationChannelStore for InMemoryNotificationChannelStore {
    async fn create(
        &self,
        new: NewNotificationChannel,
        max_channels: i64,
    ) -> Result<NotificationChannel> {
        let mut g = self.inner.lock();
        if g.iter().any(|c| c.name == new.name) {
            return Err(AppError::unprocessable(
                "CHANNEL_NAME_TAKEN",
                "a notification channel with this name already exists",
            ));
        }
        if g.len() as i64 >= max_channels {
            return Err(AppError::unprocessable(
                "CHANNEL_QUOTA_EXCEEDED",
                "notification channel limit reached for this plan",
            ));
        }
        let now = Utc::now();
        let ch = NotificationChannel {
            id: Uuid::now_v7(),
            name: new.name,
            kind: new.config.kind(),
            config: new.config,
            enabled: new.enabled,
            created_at: now,
            updated_at: now,
        };
        g.push(ch.clone());
        Ok(ch)
    }

    async fn list(&self) -> Result<Vec<NotificationChannel>> {
        let mut v = self.inner.lock().clone();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn count(&self) -> Result<u64> {
        Ok(self.inner.lock().len() as u64)
    }

    async fn get(&self, id: Uuid) -> Result<Option<NotificationChannel>> {
        Ok(self.inner.lock().iter().find(|c| c.id == id).cloned())
    }

    async fn update(
        &self,
        id: Uuid,
        update: NotificationChannelUpdate,
    ) -> Result<Option<NotificationChannel>> {
        let mut g = self.inner.lock();
        if let Some(name) = &update.name
            && g.iter().any(|c| c.id != id && &c.name == name)
        {
            return Err(AppError::unprocessable(
                "CHANNEL_NAME_TAKEN",
                "a notification channel with this name already exists",
            ));
        }
        let Some(ch) = g.iter_mut().find(|c| c.id == id) else {
            return Ok(None);
        };
        if let Some(name) = update.name {
            ch.name = name;
        }
        if let Some(cfg) = update.config {
            ch.kind = cfg.kind();
            ch.config = cfg;
        }
        if let Some(enabled) = update.enabled {
            ch.enabled = enabled;
        }
        ch.updated_at = Utc::now();
        Ok(Some(ch.clone()))
    }

    async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut g = self.inner.lock();
        let before = g.len();
        g.retain(|c| c.id != id);
        Ok(g.len() < before)
    }

    async fn existing_channel_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let g = self.inner.lock();
        Ok(ids
            .iter()
            .copied()
            .filter(|id| g.iter().any(|c| c.id == *id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn slack(name: &str) -> NewNotificationChannel {
        NewNotificationChannel {
            name: name.into(),
            config: ChannelConfig::Slack {
                webhook_url: "https://hooks.slack.com/x".into(),
            },
            enabled: true,
        }
    }

    #[tokio::test]
    async fn create_list_get_update_delete_roundtrip() {
        let store = InMemoryNotificationChannelStore::new();
        let ch = store.create(slack("ops"), 10).await.unwrap();
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert_eq!(store.get(ch.id).await.unwrap().unwrap().name, "ops");

        let patched = store
            .update(
                ch.id,
                NotificationChannelUpdate {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!patched.enabled);

        assert!(store.delete(ch.id).await.unwrap());
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn create_enforces_cap_and_unique_name() {
        let store = InMemoryNotificationChannelStore::new();
        store.create(slack("a"), 1).await.unwrap();
        let over = store.create(slack("b"), 1).await.unwrap_err();
        assert!(matches!(over, AppError::Unprocessable { .. }));
        let dup = store.create(slack("a"), 10).await.unwrap_err();
        assert!(matches!(dup, AppError::Unprocessable { .. }));
    }

    #[test]
    fn seal_open_round_trips_with_and_without_cipher() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        let cfg = ChannelConfig::Webhook {
            url: "https://x.test/h".into(),
            headers: BTreeMap::from([("X-Tok".into(), "s3cret".into())]),
        };

        // No KEK: plaintext JSON, still opens.
        let v = seal(&cfg, None).unwrap();
        assert!(v.get(crate::security::ENC_KEY).is_none());
        assert_eq!(open(v, None).unwrap(), cfg);

        // KEK: sealed envelope, no plaintext secret on disk, opens back.
        let c = Cipher::from_base64(&STANDARD.encode([5u8; 32])).unwrap();
        let sealed = seal(&cfg, Some(&c)).unwrap();
        assert!(
            sealed
                .get(crate::security::ENC_KEY)
                .unwrap()
                .as_str()
                .unwrap()
                .starts_with("v1:")
        );
        assert!(!serde_json::to_string(&sealed).unwrap().contains("s3cret"));
        assert_eq!(open(sealed, Some(&c)).unwrap(), cfg);
    }

    #[test]
    fn sealed_value_without_kek_is_loud_error() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        let c = Cipher::from_base64(&STANDARD.encode([6u8; 32])).unwrap();
        let sealed = seal(
            &ChannelConfig::Slack {
                webhook_url: "https://hooks.slack.com/x".into(),
            },
            Some(&c),
        )
        .unwrap();
        assert!(open(sealed, None).is_err());
    }
}
