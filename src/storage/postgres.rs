use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::config::PostgresConfig;
use crate::domain::{CheckSpec, NewTarget, Target, TargetUpdate};
use crate::error::Result;
use crate::storage::traits::{TargetFilter, TargetStore};

pub struct PostgresTargetStore {
    pool: PgPool,
}

impl PostgresTargetStore {
    pub async fn connect(cfg: &PostgresConfig) -> Result<Self> {
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
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
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

impl TryFrom<TargetRow> for Target {
    type Error = anyhow::Error;

    fn try_from(row: TargetRow) -> std::result::Result<Self, Self::Error> {
        let check: CheckSpec =
            serde_json::from_value(row.check_spec).context("decoding check_spec JSON")?;
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
}

fn rows_to_targets(rows: Vec<TargetRow>) -> Result<Vec<Target>> {
    rows.into_iter()
        .map(Target::try_from)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(Into::into)
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
        rows_to_targets(rows)
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
        rows_to_targets(rows)
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
            Some(r) => Ok(Some(r.try_into()?)),
            None => Ok(None),
        }
    }

    async fn create(&self, new: NewTarget) -> Result<Target> {
        let check_json = serde_json::to_value(&new.check).context("encoding check_spec JSON")?;
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
        Ok(row.try_into()?)
    }

    async fn update(&self, id: Uuid, update: TargetUpdate) -> Result<Option<Target>> {
        let check_json = update
            .check
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .context("encoding check_spec JSON")?;
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
            Some(r) => Ok(Some(r.try_into()?)),
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
        let mut tx = self.pool.begin().await.context("begin tx")?;
        let mut created = Vec::with_capacity(items.len());
        for new in items {
            let check_json =
                serde_json::to_value(&new.check).context("encoding check_spec JSON")?;
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
            .fetch_one(&mut *tx)
            .await
            .context("bulk insert target")?;
            created.push(row.try_into()?);
        }
        tx.commit().await.context("commit bulk tx")?;
        Ok(created)
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
        rows_to_targets(rows)
    }

    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("postgres ping")?;
        Ok(())
    }
}
