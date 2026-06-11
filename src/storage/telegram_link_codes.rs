//! Storage for `telegram_link_codes` — single-use codes binding a Telegram
//! chat to an org through the central bot.
//!
//! Mint is org-scoped and capped. Consume is deliberately *not* org-scoped:
//! the webhook has no tenant context — the code row IS the org authority —
//! and the guarded `UPDATE … RETURNING` makes it race-safe single-use.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};
use crate::storage::locks::{advisory_xact_lock, org_lock_key};

#[derive(Debug, Clone)]
pub struct LinkCode {
    pub id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum MintOutcome {
    Created(LinkCode),
    /// The org already holds the maximum of outstanding unconsumed codes.
    LimitReached,
}

/// What the status poll reports. A consumed row whose channel was deleted (or
/// whose channel create failed) reads as `Expired`: the code can never link
/// again, so the form should mint a fresh one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkCodeStatus {
    Pending,
    Consumed { channel_id: Uuid },
    Expired,
}

/// The org authority recovered from a consumed code — the only place the
/// webhook may take a tenant identity from.
#[derive(Debug, Clone)]
pub struct ConsumedLink {
    pub id: Uuid,
    pub org_id: OrgId,
    pub channel_name: Option<String>,
}

#[async_trait]
pub trait TelegramLinkCodeStore: Send + Sync {
    /// Capped at `max_outstanding` live codes per org, race-safe under the
    /// per-org advisory lock.
    async fn mint(
        &self,
        org: OrgId,
        created_by: Option<UserId>,
        code_hash: &str,
        channel_name: Option<&str>,
        expires_at: DateTime<Utc>,
        max_outstanding: i64,
    ) -> Result<MintOutcome>;

    /// `None` = no such code in this org.
    async fn status(&self, org: OrgId, id: Uuid) -> Result<Option<LinkCodeStatus>>;

    /// Atomically claim a live code by hash; `None` = unknown, expired, or
    /// already claimed.
    async fn consume(&self, code_hash: &str) -> Result<Option<ConsumedLink>>;

    /// Record the channel a consume produced; flips the poll to `Consumed`.
    async fn attach_channel(&self, id: Uuid, channel_id: Uuid) -> Result<()>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

pub struct PgTelegramLinkCodeStore {
    pool: PgPool,
}

impl PgTelegramLinkCodeStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TelegramLinkCodeStore for PgTelegramLinkCodeStore {
    async fn mint(
        &self,
        org: OrgId,
        created_by: Option<UserId>,
        code_hash: &str,
        channel_name: Option<&str>,
        expires_at: DateTime<Utc>,
        max_outstanding: i64,
    ) -> Result<MintOutcome> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("begin: {e}")))?;
        // Serialises the count-subquery + INSERT cap check, like every
        // other org-cap writer.
        advisory_xact_lock(&mut *tx, &org_lock_key(org))
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("advisory lock: {e}")))?;
        // Opportunistic purge of dead codes instead of a janitor.
        sqlx::query(
            r#"DELETE FROM telegram_link_codes
               WHERE org_id = $1 AND expires_at <= now() AND channel_id IS NULL"#,
        )
        .bind(org.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("purge telegram link codes: {e}")))?;
        let row: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            r#"INSERT INTO telegram_link_codes (org_id, created_by, code_hash, channel_name, expires_at)
               SELECT $1, $2, $3, $4, $5
               WHERE (SELECT count(*) FROM telegram_link_codes
                      WHERE org_id = $1 AND consumed_at IS NULL AND expires_at > now()) < $6
               RETURNING id, expires_at"#,
        )
        .bind(org.0)
        .bind(created_by.map(|u| u.0))
        .bind(code_hash)
        .bind(channel_name)
        .bind(expires_at)
        .bind(max_outstanding)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("insert telegram link code: {e}")))?;
        let Some((id, expires_at)) = row else {
            tx.rollback().await.ok();
            return Ok(MintOutcome::LimitReached);
        };
        tx.commit()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("commit: {e}")))?;
        Ok(MintOutcome::Created(LinkCode { id, expires_at }))
    }

    async fn status(&self, org: OrgId, id: Uuid) -> Result<Option<LinkCodeStatus>> {
        let row: Option<(Option<DateTime<Utc>>, DateTime<Utc>, Option<Uuid>)> = sqlx::query_as(
            r#"SELECT consumed_at, expires_at, channel_id
               FROM telegram_link_codes WHERE id = $1 AND org_id = $2"#,
        )
        .bind(id)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("telegram link code status: {e}")))?;
        Ok(row.map(|(consumed_at, expires_at, channel_id)| {
            link_status(consumed_at, expires_at, channel_id, Utc::now())
        }))
    }

    async fn consume(&self, code_hash: &str) -> Result<Option<ConsumedLink>> {
        let row: Option<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
            r#"UPDATE telegram_link_codes
               SET consumed_at = now()
               WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > now()
               RETURNING id, org_id, channel_name"#,
        )
        .bind(code_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("consume telegram link code: {e}")))?;
        Ok(row.map(|(id, org_id, channel_name)| ConsumedLink {
            id,
            org_id: OrgId(org_id),
            channel_name,
        }))
    }

    async fn attach_channel(&self, id: Uuid, channel_id: Uuid) -> Result<()> {
        sqlx::query(r#"UPDATE telegram_link_codes SET channel_id = $2 WHERE id = $1"#)
            .bind(id)
            .bind(channel_id)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("attach telegram link channel: {e}")))?;
        Ok(())
    }
}

/// Shared by the Pg and in-memory stores so they can't drift.
fn link_status(
    consumed_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    channel_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> LinkCodeStatus {
    match (consumed_at, channel_id) {
        (Some(_), Some(channel_id)) => LinkCodeStatus::Consumed { channel_id },
        // Claimed but no channel materialised: dead either way.
        (Some(_), None) => LinkCodeStatus::Expired,
        (None, _) if expires_at <= now => LinkCodeStatus::Expired,
        (None, _) => LinkCodeStatus::Pending,
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Debug, Clone)]
struct MemRow {
    id: Uuid,
    org: OrgId,
    code_hash: String,
    channel_name: Option<String>,
    expires_at: DateTime<Utc>,
    consumed_at: Option<DateTime<Utc>>,
    channel_id: Option<Uuid>,
}

#[derive(Default)]
pub struct InMemoryTelegramLinkCodeStore {
    inner: Mutex<Vec<MemRow>>,
}

impl InMemoryTelegramLinkCodeStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TelegramLinkCodeStore for InMemoryTelegramLinkCodeStore {
    async fn mint(
        &self,
        org: OrgId,
        _created_by: Option<UserId>,
        code_hash: &str,
        channel_name: Option<&str>,
        expires_at: DateTime<Utc>,
        max_outstanding: i64,
    ) -> Result<MintOutcome> {
        let mut g = self.inner.lock();
        let now = Utc::now();
        g.retain(|r| !(r.org == org && r.expires_at <= now && r.channel_id.is_none()));
        let outstanding = g
            .iter()
            .filter(|r| r.org == org && r.consumed_at.is_none() && r.expires_at > now)
            .count() as i64;
        if outstanding >= max_outstanding {
            return Ok(MintOutcome::LimitReached);
        }
        let row = MemRow {
            id: Uuid::now_v7(),
            org,
            code_hash: code_hash.to_string(),
            channel_name: channel_name.map(str::to_string),
            expires_at,
            consumed_at: None,
            channel_id: None,
        };
        let code = LinkCode {
            id: row.id,
            expires_at: row.expires_at,
        };
        g.push(row);
        Ok(MintOutcome::Created(code))
    }

    async fn status(&self, org: OrgId, id: Uuid) -> Result<Option<LinkCodeStatus>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .find(|r| r.org == org && r.id == id)
            .map(|r| link_status(r.consumed_at, r.expires_at, r.channel_id, Utc::now())))
    }

    async fn consume(&self, code_hash: &str) -> Result<Option<ConsumedLink>> {
        let mut g = self.inner.lock();
        let now = Utc::now();
        let Some(row) = g
            .iter_mut()
            .find(|r| r.code_hash == code_hash && r.consumed_at.is_none() && r.expires_at > now)
        else {
            return Ok(None);
        };
        row.consumed_at = Some(now);
        Ok(Some(ConsumedLink {
            id: row.id,
            org_id: row.org,
            channel_name: row.channel_name.clone(),
        }))
    }

    async fn attach_channel(&self, id: Uuid, channel_id: Uuid) -> Result<()> {
        if let Some(row) = self.inner.lock().iter_mut().find(|r| r.id == id) {
            row.channel_id = Some(channel_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn org() -> OrgId {
        OrgId(Uuid::from_u128(0xA1))
    }

    fn other_org() -> OrgId {
        OrgId(Uuid::from_u128(0xB2))
    }

    fn in_15_min() -> DateTime<Utc> {
        Utc::now() + Duration::minutes(15)
    }

    async fn mint(store: &InMemoryTelegramLinkCodeStore, org: OrgId, hash: &str) -> LinkCode {
        match store
            .mint(org, None, hash, None, in_15_min(), 5)
            .await
            .unwrap()
        {
            MintOutcome::Created(c) => c,
            MintOutcome::LimitReached => panic!("unexpected limit"),
        }
    }

    #[tokio::test]
    async fn consume_is_single_use() {
        let store = InMemoryTelegramLinkCodeStore::new();
        let code = mint(&store, org(), "h1").await;

        let first = store.consume("h1").await.unwrap().expect("first claim");
        assert_eq!(first.org_id, org());
        assert_eq!(first.id, code.id);
        assert!(store.consume("h1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_code_does_not_consume() {
        let store = InMemoryTelegramLinkCodeStore::new();
        store
            .mint(
                org(),
                None,
                "h1",
                None,
                Utc::now() - Duration::minutes(1),
                5,
            )
            .await
            .unwrap();
        assert!(store.consume("h1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn status_transitions_and_org_scoping() {
        let store = InMemoryTelegramLinkCodeStore::new();
        let code = mint(&store, org(), "h1").await;

        assert_eq!(
            store.status(org(), code.id).await.unwrap(),
            Some(LinkCodeStatus::Pending)
        );
        // Another org cannot observe the code at all.
        assert_eq!(store.status(other_org(), code.id).await.unwrap(), None);

        let consumed = store.consume("h1").await.unwrap().unwrap();
        // Claimed but the channel never materialised → dead, not pending.
        assert_eq!(
            store.status(org(), code.id).await.unwrap(),
            Some(LinkCodeStatus::Expired)
        );

        let channel_id = Uuid::now_v7();
        store.attach_channel(consumed.id, channel_id).await.unwrap();
        assert_eq!(
            store.status(org(), code.id).await.unwrap(),
            Some(LinkCodeStatus::Consumed { channel_id })
        );
    }

    #[tokio::test]
    async fn mint_caps_outstanding_codes_per_org() {
        let store = InMemoryTelegramLinkCodeStore::new();
        for i in 0..5 {
            mint(&store, org(), &format!("h{i}")).await;
        }
        assert!(matches!(
            store
                .mint(org(), None, "h-over", None, in_15_min(), 5)
                .await
                .unwrap(),
            MintOutcome::LimitReached
        ));
        // The cap is per org, and consuming a code frees a slot.
        mint(&store, other_org(), "h-other").await;
        store.consume("h0").await.unwrap().unwrap();
        mint(&store, org(), "h-freed").await;
    }

    #[test]
    fn unconsumed_past_expiry_reads_expired() {
        let now = Utc::now();
        assert_eq!(
            link_status(None, now - Duration::seconds(1), None, now),
            LinkCodeStatus::Expired
        );
        assert_eq!(
            link_status(None, now + Duration::seconds(1), None, now),
            LinkCodeStatus::Pending
        );
    }
}
