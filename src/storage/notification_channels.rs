//! Storage for per-org `notification_channels`.
//!
//! Every method is org-scoped (`org: OrgId`), mirroring [`super::TargetStore`].
//! One tenant can never read or mutate another's channels.
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

use crate::api::error::codes;
use crate::domain::{
    ChannelConfig, ChannelKind, NewNotificationChannel, NotificationChannel,
    NotificationChannelUpdate, OrgId, WriteSource,
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

/// Inverse of [`seal`]. Both mode mismatches — sealed row in no-KEK mode,
/// plaintext row in KEK mode — are loud errors. The plaintext-in-KEK case
/// closes the asymmetric gap a manual-INSERT path (test helper, ad-hoc SQL,
/// future demo seam) would otherwise drive through silently, since the
/// plaintext schema deserializes cleanly into `ChannelConfig` without it.
fn open(value: Value, cipher: Option<&Cipher>) -> Result<ChannelConfig> {
    match (cipher, envelope_str(&value)) {
        (Some(c), Some(env)) => {
            let bytes = c
                .decrypt(env)
                .map_err(|e| AppError::Other(anyhow!("open channel config: {e}")))?;
            serde_json::from_slice(&bytes)
                .map_err(|e| AppError::Other(anyhow!("decode channel config: {e}")))
        }
        (None, None) => serde_json::from_value(value)
            .map_err(|e| AppError::Other(anyhow!("decode channel config: {e}"))),
        (None, Some(_)) => Err(AppError::Other(anyhow!(
            "notification channel is sealed but no credentials KEK is configured"
        ))),
        (Some(_), None) => Err(AppError::Other(anyhow!(
            "notification channel is plaintext but a credentials KEK is configured — write path mismatch"
        ))),
    }
}

#[async_trait]
pub trait NotificationChannelStore: Send + Sync {
    /// Atomically capped at `max_channels` for `org`. A breach returns
    /// `CHANNEL_QUOTA_EXCEEDED`; a duplicate name (within the org)
    /// `CHANNEL_NAME_TAKEN`.
    async fn create(
        &self,
        org: OrgId,
        new: NewNotificationChannel,
        source: WriteSource,
        max_channels: i64,
    ) -> Result<NotificationChannel>;
    async fn list(&self, org: OrgId) -> Result<Vec<NotificationChannel>>;
    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<NotificationChannel>>;
    async fn update(
        &self,
        org: OrgId,
        id: Uuid,
        update: NotificationChannelUpdate,
        source: WriteSource,
    ) -> Result<Option<NotificationChannel>>;
    async fn delete(&self, org: OrgId, id: Uuid) -> Result<bool>;
    /// Disable every channel of `kind` carrying `external_ref`; returns how
    /// many flipped. Deliberately NOT org-scoped — operator-side lifecycle
    /// (a bot kick severs every org that linked the chat), keyed by a ref
    /// only the transport's own flow ever wrote.
    async fn disable_by_external_ref(
        &self,
        kind: ChannelKind,
        external_ref: &str,
        reason: &str,
    ) -> Result<u64>;
    /// Channels of `kind` (any org) still carrying `external_ref`.
    async fn count_by_external_ref(&self, kind: ChannelKind, external_ref: &str) -> Result<i64>;
    /// Subset of `ids` that exist in `org`. Mirrors
    /// [`crate::storage::MaintenanceStore::existing_target_ids`] so the
    /// "ids belong to the caller's org" idiom is uniform — used to validate
    /// target alert bindings in one query instead of N point lookups, and to
    /// close the cross-tenant IDOR where a target binds another org's channel.
    async fn existing_channel_ids(&self, org: OrgId, ids: &[Uuid]) -> Result<Vec<Uuid>>;
    /// Stamp `verified_at` on an email channel; `false` when the channel is
    /// gone, not an email kind, or was modified after `expected_updated_at`
    /// (a config swap racing the verify click must not transfer the proof).
    async fn set_verified(
        &self,
        org: OrgId,
        id: Uuid,
        expected_updated_at: DateTime<Utc>,
    ) -> Result<bool>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

/// Org-scoped Postgres store. Every query binds the caller-supplied `org` so
/// one tenant can never read or mutate another's channels.
pub struct PgNotificationChannelStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
}

impl PgNotificationChannelStore {
    pub fn new(pool: PgPool, cipher: Option<Arc<Cipher>>) -> Self {
        Self { pool, cipher }
    }
}

#[derive(sqlx::FromRow)]
struct ChannelRow {
    id: Uuid,
    name: String,
    config: Value,
    enabled: bool,
    disabled_reason: Option<String>,
    verified_at: Option<DateTime<Utc>>,
    write_source: String,
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
            disabled_reason: self.disabled_reason,
            verified_at: self.verified_at,
            write_source: WriteSource::from_db(&self.write_source),
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
        org: OrgId,
        new: NewNotificationChannel,
        source: WriteSource,
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
        advisory_xact_lock(&mut *tx, &org_lock_key(org))
            .await
            .map_err(|e| AppError::Other(anyhow!("advisory lock: {e}")))?;
        let row: Option<ChannelRow> = sqlx::query_as(
            r#"INSERT INTO notification_channels (org_id, name, kind, config, external_ref, enabled, write_source)
               SELECT $1, $2, $3, $4, $5, $6, $8
               WHERE (SELECT count(*) FROM notification_channels WHERE org_id = $1) < $7
               RETURNING id, name, config, enabled, disabled_reason, verified_at, write_source, created_at, updated_at"#,
        )
        .bind(org.0)
        .bind(&new.name)
        .bind(new.config.kind().as_db_str())
        .bind(&sealed)
        .bind(new.external_ref.as_deref())
        .bind(new.enabled)
        .bind(max_channels)
        .bind(source.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::unprocessable(
                    codes::CHANNEL_NAME_TAKEN,
                    "a notification channel with this name already exists",
                )
            } else {
                AppError::Other(anyhow!("insert notification channel: {e}"))
            }
        })?;
        let Some(row) = row else {
            tx.rollback().await.ok();
            return Err(AppError::unprocessable(
                codes::CHANNEL_QUOTA_EXCEEDED,
                "notification channel limit reached for this plan",
            ));
        };
        tx.commit()
            .await
            .map_err(|e| AppError::Other(anyhow!("commit: {e}")))?;
        row.into_channel(self.cipher.as_deref())
    }

    async fn list(&self, org: OrgId) -> Result<Vec<NotificationChannel>> {
        let rows: Vec<ChannelRow> = sqlx::query_as(
            r#"SELECT id, name, config, enabled, disabled_reason, verified_at, write_source, created_at, updated_at
               FROM notification_channels
               WHERE org_id = $1
               ORDER BY name"#,
        )
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("list notification channels: {e}")))?;
        rows.into_iter()
            .map(|r| r.into_channel(self.cipher.as_deref()))
            .collect()
    }

    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<NotificationChannel>> {
        let row: Option<ChannelRow> = sqlx::query_as(
            r#"SELECT id, name, config, enabled, disabled_reason, verified_at, write_source, created_at, updated_at
               FROM notification_channels WHERE id = $1 AND org_id = $2"#,
        )
        .bind(id)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("get notification channel: {e}")))?;
        row.map(|r| r.into_channel(self.cipher.as_deref()))
            .transpose()
    }

    async fn update(
        &self,
        org: OrgId,
        id: Uuid,
        update: NotificationChannelUpdate,
        source: WriteSource,
    ) -> Result<Option<NotificationChannel>> {
        // Re-seal only when the config is being changed; `kind` follows it.
        let (sealed, kind) = match &update.config {
            Some(c) => (
                Some(seal(c, self.cipher.as_deref())?),
                Some(c.kind().as_db_str()),
            ),
            None => (None, None),
        };
        let row: Option<ChannelRow> = sqlx::query_as(
            r#"UPDATE notification_channels
               SET name        = COALESCE($2, name),
                   kind        = COALESCE($3, kind),
                   config      = COALESCE($4, config),
                   enabled     = COALESCE($5, enabled),
                   -- Re-enabling clears the platform's disable note.
                   disabled_reason = CASE WHEN $5 THEN NULL ELSE disabled_reason END,
                   -- A replaced config must re-verify its address.
                   verified_at = CASE WHEN $4::jsonb IS NOT NULL THEN NULL ELSE verified_at END,
                   write_source = $7,
                   updated_at  = now()
               WHERE id = $1 AND org_id = $6
               RETURNING id, name, config, enabled, disabled_reason, verified_at, write_source, created_at, updated_at"#,
        )
        .bind(id)
        .bind(update.name.as_ref())
        .bind(kind)
        .bind(sealed)
        .bind(update.enabled)
        .bind(org.0)
        .bind(source.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::unprocessable(
                    codes::CHANNEL_NAME_TAKEN,
                    "a notification channel with this name already exists",
                )
            } else {
                AppError::Other(anyhow!("update notification channel: {e}"))
            }
        })?;
        row.map(|r| r.into_channel(self.cipher.as_deref()))
            .transpose()
    }

    async fn delete(&self, org: OrgId, id: Uuid) -> Result<bool> {
        let result =
            sqlx::query(r#"DELETE FROM notification_channels WHERE id = $1 AND org_id = $2"#)
                .bind(id)
                .bind(org.0)
                .execute(&self.pool)
                .await
                .map_err(|e| AppError::Other(anyhow!("delete notification channel: {e}")))?;
        Ok(result.rows_affected() > 0)
    }

    async fn disable_by_external_ref(
        &self,
        kind: ChannelKind,
        external_ref: &str,
        reason: &str,
    ) -> Result<u64> {
        let result = sqlx::query(
            r#"UPDATE notification_channels /* SAFE: operator lifecycle — a bot kick severs every org linked to the chat; external_ref is written only by the transport's own flow */
               SET enabled = false, disabled_reason = $3, updated_at = now()
               WHERE kind = $1 AND external_ref = $2 AND enabled"#,
        )
        .bind(kind.as_db_str())
        .bind(external_ref)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("disable by external ref: {e}")))?;
        Ok(result.rows_affected())
    }

    async fn count_by_external_ref(&self, kind: ChannelKind, external_ref: &str) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as(
            r#"SELECT count(*) FROM notification_channels /* SAFE: operator lifecycle — counts any org's links to a chat before the bot leaves it */
               WHERE kind = $1 AND external_ref = $2"#,
        )
        .bind(kind.as_db_str())
        .bind(external_ref)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("count by external ref: {e}")))?;
        Ok(n)
    }

    async fn existing_channel_ids(&self, org: OrgId, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"SELECT id FROM notification_channels
               WHERE id = ANY($1::uuid[]) AND org_id = $2"#,
        )
        .bind(ids)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("existing_channel_ids: {e}")))?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    async fn set_verified(
        &self,
        org: OrgId,
        id: Uuid,
        expected_updated_at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"UPDATE notification_channels
               SET verified_at = now(), updated_at = now()
               WHERE id = $1 AND org_id = $2 AND kind = 'email'
                 AND updated_at = $3"#,
        )
        .bind(id)
        .bind(org.0)
        .bind(expected_updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow!("set verified: {e}")))?;
        Ok(result.rows_affected() > 0)
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

/// Org-aware in-memory store: entries are tagged with their `OrgId` and every
/// method filters on it, so cross-tenant isolation can be asserted without a
/// Postgres backend.
struct MemEntry {
    org: OrgId,
    external_ref: Option<String>,
    ch: NotificationChannel,
}

#[derive(Default)]
pub struct InMemoryNotificationChannelStore {
    inner: Mutex<Vec<MemEntry>>,
}

impl InMemoryNotificationChannelStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total entries across every org (test introspection only).
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
        org: OrgId,
        new: NewNotificationChannel,
        source: WriteSource,
        max_channels: i64,
    ) -> Result<NotificationChannel> {
        let mut g = self.inner.lock();
        if g.iter().any(|e| e.org == org && e.ch.name == new.name) {
            return Err(AppError::unprocessable(
                codes::CHANNEL_NAME_TAKEN,
                "a notification channel with this name already exists",
            ));
        }
        if g.iter().filter(|e| e.org == org).count() as i64 >= max_channels {
            return Err(AppError::unprocessable(
                codes::CHANNEL_QUOTA_EXCEEDED,
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
            disabled_reason: None,
            verified_at: None,
            write_source: source,
            created_at: now,
            updated_at: now,
        };
        g.push(MemEntry {
            org,
            external_ref: new.external_ref,
            ch: ch.clone(),
        });
        Ok(ch)
    }

    async fn list(&self, org: OrgId) -> Result<Vec<NotificationChannel>> {
        let mut v: Vec<NotificationChannel> = self
            .inner
            .lock()
            .iter()
            .filter(|e| e.org == org)
            .map(|e| e.ch.clone())
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<NotificationChannel>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .find(|e| e.org == org && e.ch.id == id)
            .map(|e| e.ch.clone()))
    }

    async fn update(
        &self,
        org: OrgId,
        id: Uuid,
        update: NotificationChannelUpdate,
        source: WriteSource,
    ) -> Result<Option<NotificationChannel>> {
        let mut g = self.inner.lock();
        if let Some(name) = &update.name
            && g.iter()
                .any(|e| e.org == org && e.ch.id != id && &e.ch.name == name)
        {
            return Err(AppError::unprocessable(
                codes::CHANNEL_NAME_TAKEN,
                "a notification channel with this name already exists",
            ));
        }
        let Some(entry) = g.iter_mut().find(|e| e.org == org && e.ch.id == id) else {
            return Ok(None);
        };
        let ch = &mut entry.ch;
        if let Some(name) = update.name {
            ch.name = name;
        }
        if let Some(cfg) = update.config {
            ch.kind = cfg.kind();
            ch.config = cfg;
            ch.verified_at = None;
        }
        if let Some(enabled) = update.enabled {
            ch.enabled = enabled;
            if enabled {
                ch.disabled_reason = None;
            }
        }
        ch.write_source = source;
        ch.updated_at = Utc::now();
        Ok(Some(ch.clone()))
    }

    async fn delete(&self, org: OrgId, id: Uuid) -> Result<bool> {
        let mut g = self.inner.lock();
        let before = g.len();
        g.retain(|e| !(e.org == org && e.ch.id == id));
        Ok(g.len() < before)
    }

    async fn disable_by_external_ref(
        &self,
        kind: ChannelKind,
        external_ref: &str,
        reason: &str,
    ) -> Result<u64> {
        let mut flipped = 0;
        for e in self.inner.lock().iter_mut() {
            if e.ch.kind == kind && e.external_ref.as_deref() == Some(external_ref) && e.ch.enabled
            {
                e.ch.enabled = false;
                e.ch.disabled_reason = Some(reason.to_string());
                e.ch.updated_at = Utc::now();
                flipped += 1;
            }
        }
        Ok(flipped)
    }

    async fn count_by_external_ref(&self, kind: ChannelKind, external_ref: &str) -> Result<i64> {
        Ok(self
            .inner
            .lock()
            .iter()
            .filter(|e| e.ch.kind == kind && e.external_ref.as_deref() == Some(external_ref))
            .count() as i64)
    }

    async fn existing_channel_ids(&self, org: OrgId, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let g = self.inner.lock();
        Ok(ids
            .iter()
            .copied()
            .filter(|id| g.iter().any(|e| e.org == org && e.ch.id == *id))
            .collect())
    }

    async fn set_verified(
        &self,
        org: OrgId,
        id: Uuid,
        expected_updated_at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut g = self.inner.lock();
        let Some(e) = g.iter_mut().find(|e| {
            e.org == org
                && e.ch.id == id
                && e.ch.kind == ChannelKind::Email
                && e.ch.updated_at == expected_updated_at
        }) else {
            return Ok(false);
        };
        e.ch.verified_at = Some(Utc::now());
        e.ch.updated_at = Utc::now();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SlackConfig, WebhookConfig};
    use std::collections::BTreeMap;

    fn org() -> OrgId {
        OrgId(Uuid::from_u128(0xA1))
    }

    fn other_org() -> OrgId {
        OrgId(Uuid::from_u128(0xB2))
    }

    fn slack(name: &str) -> NewNotificationChannel {
        NewNotificationChannel {
            name: name.into(),
            config: ChannelConfig::Slack(SlackConfig {
                webhook_url: "https://hooks.slack.com/x".into(),
            }),
            enabled: true,
            external_ref: None,
        }
    }

    #[tokio::test]
    async fn create_list_get_update_delete_roundtrip() {
        let store = InMemoryNotificationChannelStore::new();
        let ch = store
            .create(org(), slack("ops"), WriteSource::Ui, 10)
            .await
            .unwrap();
        assert_eq!(store.list(org()).await.unwrap().len(), 1);
        assert_eq!(store.get(org(), ch.id).await.unwrap().unwrap().name, "ops");

        let patched = store
            .update(
                org(),
                ch.id,
                NotificationChannelUpdate {
                    enabled: Some(false),
                    ..Default::default()
                },
                WriteSource::Ui,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!patched.enabled);

        assert!(store.delete(org(), ch.id).await.unwrap());
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn create_enforces_cap_and_unique_name() {
        let store = InMemoryNotificationChannelStore::new();
        store
            .create(org(), slack("a"), WriteSource::Ui, 1)
            .await
            .unwrap();
        let over = store
            .create(org(), slack("b"), WriteSource::Ui, 1)
            .await
            .unwrap_err();
        assert!(matches!(over, AppError::Unprocessable { .. }));
        let dup = store
            .create(org(), slack("a"), WriteSource::Ui, 10)
            .await
            .unwrap_err();
        assert!(matches!(dup, AppError::Unprocessable { .. }));
    }

    #[tokio::test]
    async fn channels_are_isolated_per_org() {
        let store = InMemoryNotificationChannelStore::new();
        let a = store
            .create(org(), slack("ops"), WriteSource::Ui, 10)
            .await
            .unwrap();
        // Same name in a different org is allowed and invisible to org A.
        store
            .create(other_org(), slack("ops"), WriteSource::Ui, 10)
            .await
            .unwrap();

        assert_eq!(store.list(org()).await.unwrap().len(), 1);
        assert_eq!(store.list(other_org()).await.unwrap().len(), 1);
        // org B cannot read, mutate, or delete org A's channel by id.
        assert!(store.get(other_org(), a.id).await.unwrap().is_none());
        assert!(
            store
                .update(
                    other_org(),
                    a.id,
                    NotificationChannelUpdate::default(),
                    WriteSource::Ui
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(!store.delete(other_org(), a.id).await.unwrap());
        assert!(
            store
                .existing_channel_ids(other_org(), &[a.id])
                .await
                .unwrap()
                .is_empty()
        );
        // Still intact for the owning org.
        assert!(store.get(org(), a.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn lifecycle_disable_by_external_ref_spans_orgs_and_reenable_clears_note() {
        let store = InMemoryNotificationChannelStore::new();
        let linked = |name: &str| NewNotificationChannel {
            name: name.into(),
            config: ChannelConfig::TelegramApp(crate::domain::TelegramAppConfig {
                chat_id: "-100".into(),
                chat_title: None,
            }),
            enabled: true,
            external_ref: Some("-100".into()),
        };
        // Two orgs linked the same chat; a third channel points elsewhere.
        let a = store
            .create(org(), linked("prod"), WriteSource::Ui, 10)
            .await
            .unwrap();
        store
            .create(other_org(), linked("ops"), WriteSource::Ui, 10)
            .await
            .unwrap();
        let mut other = linked("other-chat");
        other.external_ref = Some("-200".into());
        store
            .create(org(), other, WriteSource::Ui, 10)
            .await
            .unwrap();

        assert_eq!(
            store
                .count_by_external_ref(ChannelKind::TelegramApp, "-100")
                .await
                .unwrap(),
            2
        );
        let flipped = store
            .disable_by_external_ref(ChannelKind::TelegramApp, "-100", "unlinked")
            .await
            .unwrap();
        assert_eq!(flipped, 2, "both orgs' channels for the kicked chat");
        // Idempotent: already-disabled rows don't flip again.
        assert_eq!(
            store
                .disable_by_external_ref(ChannelKind::TelegramApp, "-100", "unlinked")
                .await
                .unwrap(),
            0
        );
        let got = store.get(org(), a.id).await.unwrap().unwrap();
        assert!(!got.enabled);
        assert_eq!(got.disabled_reason.as_deref(), Some("unlinked"));
        // The unrelated chat's channel is untouched.
        assert_eq!(
            store
                .list(org())
                .await
                .unwrap()
                .iter()
                .filter(|c| c.enabled)
                .count(),
            1
        );

        // Re-enabling clears the platform note; disabling again does not
        // resurrect it.
        let re = store
            .update(
                org(),
                a.id,
                NotificationChannelUpdate {
                    enabled: Some(true),
                    ..Default::default()
                },
                WriteSource::Ui,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(re.enabled);
        assert_eq!(re.disabled_reason, None);
    }

    #[test]
    fn seal_open_round_trips_with_and_without_cipher() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        let cfg = ChannelConfig::Webhook(WebhookConfig {
            url: "https://x.test/h".into(),
            headers: BTreeMap::from([("X-Tok".into(), "s3cret".into())]),
            secret: None,
        });

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
            &ChannelConfig::Slack(SlackConfig {
                webhook_url: "https://hooks.slack.com/x".into(),
            }),
            Some(&c),
        )
        .unwrap();
        assert!(open(sealed, None).is_err());
    }

    #[test]
    fn plaintext_value_with_kek_is_loud_error() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        let c = Cipher::from_base64(&STANDARD.encode([7u8; 32])).unwrap();
        // Simulates a manual-INSERT path (test helper, ad-hoc SQL, demo seam)
        // that wrote a plaintext config into a KEK-mode deployment.
        let plaintext = seal(
            &ChannelConfig::Slack(SlackConfig {
                webhook_url: "https://hooks.slack.com/x".into(),
            }),
            None,
        )
        .unwrap();
        assert!(open(plaintext, Some(&c)).is_err());
    }
}
