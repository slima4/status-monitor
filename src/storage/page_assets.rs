//! Storage for `page_assets` — per-status-page resources (logo, and later
//! background/favicon/css) stored inline as BYTEA. The `storage`/`external_key`
//! columns are the seam to move bytes to S3 later; this store only reads/writes
//! the `db` path. get/delete are page-scoped (the page id is already org-scoped
//! upstream by the owner extractor); `put` takes the org for the row.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::domain::{AssetSlot, OrgId, StatusPageId};
use crate::error::{AppError, Result};

/// Stored asset bytes + how to serve them.
#[derive(Debug, Clone)]
pub struct AssetContent {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub content_hash: String,
}

/// Asset metadata without the bytes — for the cache-bust URL + existence checks.
#[derive(Debug, Clone)]
pub struct AssetMeta {
    pub content_hash: String,
    pub content_type: String,
    pub byte_size: i64,
}

#[async_trait]
pub trait PageAssetStore: Send + Sync {
    /// UPSERT the slot's bytes (one row per `(page, slot)`). `content_hash` is
    /// `hex(sha256(bytes))`; `byte_size` is `bytes.len()`. Always writes the
    /// `db` storage path. Returns the stored metadata.
    async fn put(
        &self,
        org: OrgId,
        page: StatusPageId,
        slot: AssetSlot,
        content_type: &str,
        bytes: &[u8],
        metadata: serde_json::Value,
    ) -> Result<AssetMeta>;
    /// Read the bytes for serving. Only the `db` storage path is read; a future
    /// `s3` row returns `None` (the bytes live elsewhere, not yet wired).
    async fn get(&self, page: StatusPageId, slot: AssetSlot) -> Result<Option<AssetContent>>;
    /// Metadata only (no bytes) for the `db` storage path.
    async fn get_meta(&self, page: StatusPageId, slot: AssetSlot) -> Result<Option<AssetMeta>>;
    async fn delete(&self, page: StatusPageId, slot: AssetSlot) -> Result<bool>;
}

pub struct PgPageAssetStore {
    pool: PgPool,
}

impl PgPageAssetStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

fn db_err(e: sqlx::Error) -> AppError {
    AppError::Other(anyhow::anyhow!("page_assets: {e}"))
}

#[async_trait]
impl PageAssetStore for PgPageAssetStore {
    async fn put(
        &self,
        org: OrgId,
        page: StatusPageId,
        slot: AssetSlot,
        content_type: &str,
        bytes: &[u8],
        metadata: serde_json::Value,
    ) -> Result<AssetMeta> {
        let hash = content_hash(bytes);
        let byte_size = bytes.len() as i64;
        sqlx::query(
            r#"INSERT INTO page_assets
                   (org_id, status_page_id, slot, content_type, content_hash,
                    byte_size, storage, data, external_key, metadata)
               VALUES ($1, $2, $3, $4, $5, $6, 'db', $7, NULL, $8)
               ON CONFLICT (status_page_id, slot) DO UPDATE SET
                   org_id       = EXCLUDED.org_id,
                   content_type = EXCLUDED.content_type,
                   content_hash = EXCLUDED.content_hash,
                   byte_size    = EXCLUDED.byte_size,
                   storage      = 'db',
                   data         = EXCLUDED.data,
                   external_key = NULL,
                   metadata     = EXCLUDED.metadata,
                   updated_at   = now()"#,
        )
        .bind(org.0)
        .bind(page.0)
        .bind(slot.as_str())
        .bind(content_type)
        .bind(&hash)
        .bind(byte_size)
        .bind(bytes)
        .bind(metadata)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(AssetMeta {
            content_hash: hash,
            content_type: content_type.to_owned(),
            byte_size,
        })
    }

    async fn get(&self, page: StatusPageId, slot: AssetSlot) -> Result<Option<AssetContent>> {
        let row: Option<(Vec<u8>, String, String)> = sqlx::query_as(
            "SELECT data, content_type, content_hash FROM page_assets \
             WHERE status_page_id = $1 AND slot = $2 AND storage = 'db'",
        )
        .bind(page.0)
        .bind(slot.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(bytes, content_type, content_hash)| AssetContent {
            bytes,
            content_type,
            content_hash,
        }))
    }

    async fn get_meta(&self, page: StatusPageId, slot: AssetSlot) -> Result<Option<AssetMeta>> {
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT content_hash, content_type, byte_size FROM page_assets \
             WHERE status_page_id = $1 AND slot = $2 AND storage = 'db'",
        )
        .bind(page.0)
        .bind(slot.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(
            row.map(|(content_hash, content_type, byte_size)| AssetMeta {
                content_hash,
                content_type,
                byte_size,
            }),
        )
    }

    async fn delete(&self, page: StatusPageId, slot: AssetSlot) -> Result<bool> {
        let res = sqlx::query("DELETE FROM page_assets WHERE status_page_id = $1 AND slot = $2")
            .bind(page.0)
            .bind(slot.as_str())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }
}

// ── In-memory store (tests / no-DB harnesses) ────────────────────────────────

/// In-memory [`PageAssetStore`] for no-DB fixtures. Keyed by `(page, slot)`,
/// mirroring the `db` storage path of [`PgPageAssetStore`].
#[derive(Default)]
pub struct InMemoryPageAssetStore {
    inner: std::sync::Mutex<std::collections::HashMap<(uuid::Uuid, &'static str), AssetContent>>,
}

impl InMemoryPageAssetStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PageAssetStore for InMemoryPageAssetStore {
    async fn put(
        &self,
        _org: OrgId,
        page: StatusPageId,
        slot: AssetSlot,
        content_type: &str,
        bytes: &[u8],
        _metadata: serde_json::Value,
    ) -> Result<AssetMeta> {
        let hash = content_hash(bytes);
        let byte_size = bytes.len() as i64;
        self.inner.lock().unwrap().insert(
            (page.0, slot.as_str()),
            AssetContent {
                bytes: bytes.to_vec(),
                content_type: content_type.to_owned(),
                content_hash: hash.clone(),
            },
        );
        Ok(AssetMeta {
            content_hash: hash,
            content_type: content_type.to_owned(),
            byte_size,
        })
    }

    async fn get(&self, page: StatusPageId, slot: AssetSlot) -> Result<Option<AssetContent>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(page.0, slot.as_str()))
            .cloned())
    }

    async fn get_meta(&self, page: StatusPageId, slot: AssetSlot) -> Result<Option<AssetMeta>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(page.0, slot.as_str()))
            .map(|c| AssetMeta {
                content_hash: c.content_hash.clone(),
                content_type: c.content_type.clone(),
                byte_size: c.bytes.len() as i64,
            }))
    }

    async fn delete(&self, page: StatusPageId, slot: AssetSlot) -> Result<bool> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .remove(&(page.0, slot.as_str()))
            .is_some())
    }
}
