use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use uuid::Uuid;

use crate::config::PostgresConfig;
use crate::domain::{CheckSpec, NewTarget, Target, TargetUpdate};
use crate::error::Result;
use crate::security::Cipher;
use crate::storage::postgres_secrets::{decrypt_in_place, encrypt_in_place};
use crate::storage::traits::{TargetFilter, TargetStore};

pub struct PostgresTargetStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
}

impl PostgresTargetStore {
    pub async fn connect(cfg: &PostgresConfig, cipher: Option<Arc<Cipher>>) -> Result<Self> {
        tracing::info!(
            max_connections = cfg.max_connections,
            min_connections = cfg.min_connections,
            "connecting to postgres"
        );
        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .min_connections(cfg.min_connections)
            .acquire_timeout(Duration::from_secs(cfg.acquire_timeout_secs))
            .connect(&cfg.url)
            .await
            .context("failed to connect to postgres")?;
        tracing::info!("running postgres migrations");
        sqlx::migrate!("./migrations/postgres")
            .run(&pool)
            .await
            .context("postgres migrations")?;
        tracing::info!("postgres ready");
        Ok(Self { pool, cipher })
    }

    pub fn from_pool(pool: PgPool, cipher: Option<Arc<Cipher>>) -> Self {
        Self { pool, cipher }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn encode_check(&self, check: &CheckSpec) -> Result<serde_json::Value> {
        let mut v = serde_json::to_value(check).context("encoding check_spec JSON")?;
        if let Some(cipher) = &self.cipher {
            encrypt_in_place(&mut v, cipher)?;
        }
        Ok(v)
    }

    fn decode_row(&self, row: TargetRow) -> Result<Target> {
        let mut check_json = row.check_spec;
        if let Some(cipher) = &self.cipher {
            decrypt_in_place(&mut check_json, cipher, row.id)?;
        }
        let check: CheckSpec =
            serde_json::from_value(check_json).context("decoding check_spec JSON")?;
        Ok(Target {
            id: row.id,
            name: row.name,
            check,
            interval: Duration::from_secs(row.interval_secs.max(0) as u64),
            enabled: row.enabled,
            tags: row.tags,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    fn rows_to_targets(&self, rows: Vec<TargetRow>) -> Result<Vec<Target>> {
        rows.into_iter().map(|r| self.decode_row(r)).collect()
    }
}

#[derive(sqlx::FromRow)]
struct TargetRow {
    id: Uuid,
    name: String,
    check_spec: serde_json::Value,
    interval_secs: i32,
    enabled: bool,
    tags: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[async_trait]
impl TargetStore for PostgresTargetStore {
    async fn list(&self, filter: TargetFilter) -> Result<Vec<Target>> {
        let limit = filter.limit.unwrap_or(100).min(10_000) as i64;
        let offset = filter.offset as i64;
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at
               FROM targets
               WHERE ($1::bool IS NULL OR enabled = $1)
                 AND ($2::text IS NULL OR $2 = ANY(tags))
               ORDER BY created_at DESC
               LIMIT $3 OFFSET $4"#,
        )
        .bind(filter.enabled)
        .bind(filter.tag)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .context("query targets")?;
        self.rows_to_targets(rows)
    }

    async fn list_enabled(&self) -> Result<Vec<Target>> {
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at
               FROM targets
               WHERE enabled = true"#,
        )
        .fetch_all(&self.pool)
        .await
        .context("query enabled targets")?;
        self.rows_to_targets(rows)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Target>> {
        let row: Option<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at
               FROM targets WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("get target by id")?;
        match row {
            Some(r) => Ok(Some(self.decode_row(r)?)),
            None => Ok(None),
        }
    }

    async fn create(&self, new: NewTarget) -> Result<Target> {
        let check_json = self.encode_check(&new.check)?;
        let row: TargetRow = sqlx::query_as::<_, TargetRow>(
            r#"INSERT INTO targets (name, check_spec, interval_secs, enabled, tags)
               VALUES ($1, $2, $3, $4, $5)
               RETURNING id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at"#,
        )
        .bind(&new.name)
        .bind(check_json)
        .bind(new.interval.as_secs() as i32)
        .bind(new.enabled)
        .bind(&new.tags)
        .fetch_one(&self.pool)
        .await
        .context("insert target")?;
        self.decode_row(row)
    }

    async fn update(&self, id: Uuid, update: TargetUpdate) -> Result<Option<Target>> {
        let check_json = update
            .check
            .as_ref()
            .map(|c| self.encode_check(c))
            .transpose()?;
        let interval_secs = update.interval.map(|d| d.as_secs() as i32);

        let row: Option<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"UPDATE targets SET
                 name = COALESCE($2, name),
                 check_spec = COALESCE($3, check_spec),
                 interval_secs = COALESCE($4, interval_secs),
                 enabled = COALESCE($5, enabled),
                 tags = COALESCE($6, tags),
                 updated_at = now()
               WHERE id = $1
               RETURNING id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at"#,
        )
        .bind(id)
        .bind(update.name)
        .bind(check_json)
        .bind(interval_secs)
        .bind(update.enabled)
        .bind(update.tags)
        .fetch_optional(&self.pool)
        .await
        .context("update target")?;
        match row {
            Some(r) => Ok(Some(self.decode_row(r)?)),
            None => Ok(None),
        }
    }

    async fn delete(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM targets WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete target")?;
        Ok(res.rows_affected() > 0)
    }

    async fn bulk_create(&self, items: Vec<NewTarget>) -> Result<Vec<Target>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        const SQL: &str = r#"INSERT INTO targets (name, check_spec, interval_secs, enabled, tags)
               SELECT u.name, u.check_spec, u.interval_secs, u.enabled,
                      ARRAY(SELECT jsonb_array_elements_text(u.tags))
               FROM UNNEST($1::text[], $2::jsonb[], $3::int4[], $4::bool[], $5::jsonb[])
                    AS u(name, check_spec, interval_secs, enabled, tags)
               RETURNING id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at"#;

        let len = items.len();
        let mut names: Vec<String> = Vec::with_capacity(len);
        let mut intervals: Vec<i32> = Vec::with_capacity(len);
        let mut enabled: Vec<bool> = Vec::with_capacity(len);
        // Postgres rejects ragged 2-D arrays (text[][] must be rectangular), so
        // pass per-row tag lists as jsonb and unpack on the server side.
        let mut tags_json: Vec<Json<Vec<String>>> = Vec::with_capacity(len);

        let rows: Vec<TargetRow> = if let Some(cipher) = &self.cipher {
            // Cipher path: walk each CheckSpec via serde_json::Value so credential
            // fields can be wrapped before binding.
            let mut check_specs: Vec<Json<serde_json::Value>> = Vec::with_capacity(len);
            for new in items {
                let mut v = serde_json::to_value(&new.check).context("encoding check_spec JSON")?;
                encrypt_in_place(&mut v, cipher)?;
                names.push(new.name);
                check_specs.push(Json(v));
                intervals.push(new.interval.as_secs() as i32);
                enabled.push(new.enabled);
                tags_json.push(Json(new.tags));
            }
            sqlx::query_as::<_, TargetRow>(SQL)
                .bind(&names)
                .bind(&check_specs)
                .bind(&intervals)
                .bind(&enabled)
                .bind(&tags_json)
                .fetch_all(&self.pool)
                .await
                .context("bulk insert targets")?
        } else {
            // Plaintext path: bind CheckSpec directly so sqlx serializes straight to
            // wire bytes, skipping the intermediate Value tree.
            let mut check_specs: Vec<Json<CheckSpec>> = Vec::with_capacity(len);
            for new in items {
                names.push(new.name);
                check_specs.push(Json(new.check));
                intervals.push(new.interval.as_secs() as i32);
                enabled.push(new.enabled);
                tags_json.push(Json(new.tags));
            }
            sqlx::query_as::<_, TargetRow>(SQL)
                .bind(&names)
                .bind(&check_specs)
                .bind(&intervals)
                .bind(&enabled)
                .bind(&tags_json)
                .fetch_all(&self.pool)
                .await
                .context("bulk insert targets")?
        };

        self.rows_to_targets(rows)
    }

    async fn list_updated_since(&self, since: DateTime<Utc>) -> Result<Vec<Target>> {
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, created_at, updated_at
               FROM targets WHERE updated_at > $1"#,
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("query targets updated since")?;
        self.rows_to_targets(rows)
    }

    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("postgres ping")?;
        Ok(())
    }
}
