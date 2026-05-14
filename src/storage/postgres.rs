use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use uuid::Uuid;

use crate::api::types::{TagCount, TargetsSummary};
use crate::config::PostgresConfig;
use crate::domain::{CheckSpec, NewTarget, OrgId, Target, TargetAlerts, TargetUpdate};
use crate::error::Result;
use crate::security::Cipher;
use crate::storage::postgres_secrets::{decrypt_in_place, encrypt_in_place};
use crate::storage::traits::{TargetFilter, TargetStore};

pub struct PostgresTargetStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
    /// Org id every insert is stamped with. Phase 2 will replace this with an
    /// `OrgId` parameter on each trait method.
    default_org_id: OrgId,
}

impl PostgresTargetStore {
    /// Open the pool and run Postgres migrations. Returns just the pool so
    /// startup can provision the default org before constructing the store.
    pub async fn connect_pool(cfg: &PostgresConfig) -> Result<PgPool> {
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
        Ok(pool)
    }

    pub fn from_pool(pool: PgPool, cipher: Option<Arc<Cipher>>, default_org_id: OrgId) -> Self {
        Self { pool, cipher, default_org_id }
    }

    fn org_id(&self) -> Uuid {
        self.default_org_id.0
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
        let alerts: TargetAlerts =
            serde_json::from_value(row.alerts).context("decoding alerts JSON")?;
        Ok(Target {
            id: row.id,
            name: row.name,
            check,
            interval: Duration::from_secs(row.interval_secs.max(0) as u64),
            enabled: row.enabled,
            tags: row.tags,
            alerts,
            public_status: row.public_status,
            public_name: row.public_name,
            public_description: row.public_description,
            public_group: row.public_group,
            public_sort_order: row.public_sort_order,
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
    alerts: serde_json::Value,
    public_status: bool,
    public_name: Option<String>,
    public_description: Option<String>,
    public_group: Option<String>,
    public_sort_order: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}


#[async_trait]
impl TargetStore for PostgresTargetStore {
    async fn list(&self, filter: TargetFilter) -> Result<Vec<Target>> {
        let limit = filter.limit.unwrap_or(100).min(10_000) as i64;
        let offset = filter.offset as i64;
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
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

    async fn count(&self, filter: TargetFilter) -> Result<u64> {
        let row: (i64,) = sqlx::query_as(
            r#"SELECT count(*) FROM targets
               WHERE ($1::bool IS NULL OR enabled = $1)
                 AND ($2::text IS NULL OR $2 = ANY(tags))"#,
        )
        .bind(filter.enabled)
        .bind(filter.tag)
        .fetch_one(&self.pool)
        .await
        .context("count targets")?;
        Ok(row.0.max(0) as u64)
    }

    async fn list_enabled(&self) -> Result<Vec<Target>> {
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
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
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
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
        let alerts_json = serde_json::to_value(&new.alerts).context("encoding alerts JSON")?;
        let row: TargetRow = sqlx::query_as::<_, TargetRow>(
            r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled, tags, alerts,
                                    public_status, public_name, public_description, public_group, public_sort_order)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
               RETURNING id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at"#,
        )
        .bind(self.org_id())
        .bind(&new.name)
        .bind(check_json)
        .bind(new.interval.as_secs() as i32)
        .bind(new.enabled)
        .bind(&new.tags)
        .bind(alerts_json)
        .bind(new.public_status)
        .bind(&new.public_name)
        .bind(&new.public_description)
        .bind(&new.public_group)
        .bind(new.public_sort_order)
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
        let alerts_json = update
            .alerts
            .as_ref()
            .map(|a| serde_json::to_value(a).context("encoding alerts JSON"))
            .transpose()?;

        let row: Option<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"UPDATE targets SET
                 name = COALESCE($2, name),
                 check_spec = COALESCE($3, check_spec),
                 interval_secs = COALESCE($4, interval_secs),
                 enabled = COALESCE($5, enabled),
                 tags = COALESCE($6, tags),
                 alerts = COALESCE($7, alerts),
                 public_status = COALESCE($8, public_status),
                 public_name = COALESCE($9, public_name),
                 public_description = COALESCE($10, public_description),
                 public_group = COALESCE($11, public_group),
                 public_sort_order = COALESCE($12, public_sort_order),
                 updated_at = now()
               WHERE id = $1
               RETURNING id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at"#,
        )
        .bind(id)
        .bind(update.name)
        .bind(check_json)
        .bind(interval_secs)
        .bind(update.enabled)
        .bind(update.tags)
        .bind(alerts_json)
        .bind(update.public_status)
        .bind(update.public_name)
        .bind(update.public_description)
        .bind(update.public_group)
        .bind(update.public_sort_order)
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

        const SQL: &str = r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled, tags, alerts,
                                    public_status, public_name, public_description, public_group, public_sort_order)
               SELECT $12, u.name, u.check_spec, u.interval_secs, u.enabled,
                      ARRAY(SELECT jsonb_array_elements_text(u.tags)),
                      u.alerts,
                      u.public_status, u.public_name, u.public_description, u.public_group, u.public_sort_order
               FROM UNNEST($1::text[], $2::jsonb[], $3::int4[], $4::bool[], $5::jsonb[], $6::jsonb[],
                           $7::bool[], $8::text[], $9::text[], $10::text[], $11::int4[])
                    AS u(name, check_spec, interval_secs, enabled, tags, alerts,
                         public_status, public_name, public_description, public_group, public_sort_order)
               RETURNING id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at"#;

        let len = items.len();
        let mut names: Vec<String> = Vec::with_capacity(len);
        let mut intervals: Vec<i32> = Vec::with_capacity(len);
        let mut enabled: Vec<bool> = Vec::with_capacity(len);
        // Postgres rejects ragged 2-D arrays (text[][] must be rectangular), so
        // pass per-row tag lists as jsonb and unpack on the server side.
        let mut tags_json: Vec<Json<Vec<String>>> = Vec::with_capacity(len);
        let mut alerts_json: Vec<Json<TargetAlerts>> = Vec::with_capacity(len);
        let mut public_status: Vec<bool> = Vec::with_capacity(len);
        let mut public_name: Vec<Option<String>> = Vec::with_capacity(len);
        let mut public_description: Vec<Option<String>> = Vec::with_capacity(len);
        let mut public_group: Vec<Option<String>> = Vec::with_capacity(len);
        let mut public_sort_order: Vec<i32> = Vec::with_capacity(len);

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
                alerts_json.push(Json(new.alerts));
                public_status.push(new.public_status);
                public_name.push(new.public_name);
                public_description.push(new.public_description);
                public_group.push(new.public_group);
                public_sort_order.push(new.public_sort_order);
            }
            sqlx::query_as::<_, TargetRow>(SQL)
                .bind(&names)
                .bind(&check_specs)
                .bind(&intervals)
                .bind(&enabled)
                .bind(&tags_json)
                .bind(&alerts_json)
                .bind(&public_status)
                .bind(&public_name)
                .bind(&public_description)
                .bind(&public_group)
                .bind(&public_sort_order)
                .bind(self.org_id())
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
                alerts_json.push(Json(new.alerts));
                public_status.push(new.public_status);
                public_name.push(new.public_name);
                public_description.push(new.public_description);
                public_group.push(new.public_group);
                public_sort_order.push(new.public_sort_order);
            }
            sqlx::query_as::<_, TargetRow>(SQL)
                .bind(&names)
                .bind(&check_specs)
                .bind(&intervals)
                .bind(&enabled)
                .bind(&tags_json)
                .bind(&alerts_json)
                .bind(&public_status)
                .bind(&public_name)
                .bind(&public_description)
                .bind(&public_group)
                .bind(&public_sort_order)
                .bind(self.org_id())
                .fetch_all(&self.pool)
                .await
                .context("bulk insert targets")?
        };

        self.rows_to_targets(rows)
    }

    async fn list_updated_since(&self, since: DateTime<Utc>) -> Result<Vec<Target>> {
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
               FROM targets WHERE updated_at > $1"#,
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("query targets updated since")?;
        self.rows_to_targets(rows)
    }

    async fn list_tags(&self, prefix: Option<String>, limit: usize) -> Result<Vec<TagCount>> {
        let prefix_pat = prefix.as_deref().map(|p| format!("{p}%"));
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"SELECT tag, count(*) AS c
               FROM targets, unnest(tags) AS tag
               WHERE ($1::text IS NULL OR tag LIKE $1)
               GROUP BY tag
               ORDER BY c DESC, tag ASC
               LIMIT $2"#,
        )
        .bind(prefix_pat)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("aggregate tag counts")?;
        Ok(rows
            .into_iter()
            .map(|(name, c)| TagCount {
                name,
                count: c.max(0) as u64,
            })
            .collect())
    }

    async fn count_tags(&self, prefix: Option<String>) -> Result<u64> {
        let prefix_pat = prefix.as_deref().map(|p| format!("{p}%"));
        let row: (i64,) = sqlx::query_as(
            r#"SELECT count(DISTINCT tag)
               FROM targets, unnest(tags) AS tag
               WHERE ($1::text IS NULL OR tag LIKE $1)"#,
        )
        .bind(prefix_pat)
        .fetch_one(&self.pool)
        .await
        .context("count distinct tags")?;
        Ok(row.0.max(0) as u64)
    }

    async fn summary(&self) -> Result<TargetsSummary> {
        let row: (i64, i64) = sqlx::query_as(
            r#"SELECT count(*) FILTER (WHERE TRUE),
                      count(*) FILTER (WHERE enabled)
               FROM targets"#,
        )
        .fetch_one(&self.pool)
        .await
        .context("targets summary")?;
        let total = row.0.max(0) as u64;
        let enabled = row.1.max(0) as u64;
        Ok(TargetsSummary {
            total,
            enabled,
            disabled: total - enabled,
        })
    }

    async fn set_enabled(&self, ids: &[Uuid], enabled: bool) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"UPDATE targets SET enabled = $2, updated_at = now()
               WHERE id = ANY($1) RETURNING id"#,
        )
        .bind(ids)
        .bind(enabled)
        .fetch_all(&self.pool)
        .await
        .context("bulk set_enabled")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn delete_bulk(&self, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"DELETE FROM targets WHERE id = ANY($1) RETURNING id"#,
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .context("bulk delete")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn add_tags(&self, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>> {
        if ids.is_empty() || tags.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"UPDATE targets
               SET tags = (
                 SELECT array_agg(DISTINCT t)
                 FROM unnest(tags || $2) AS t
               ),
               updated_at = now()
               WHERE id = ANY($1)
               RETURNING id"#,
        )
        .bind(ids)
        .bind(tags)
        .fetch_all(&self.pool)
        .await
        .context("bulk add_tags")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn remove_tags(&self, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>> {
        if ids.is_empty() || tags.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"UPDATE targets
               SET tags = COALESCE((
                 SELECT array_agg(t)
                 FROM unnest(tags) AS t
                 WHERE NOT (t = ANY($2))
               ), ARRAY[]::text[]),
               updated_at = now()
               WHERE id = ANY($1)
               RETURNING id"#,
        )
        .bind(ids)
        .bind(tags)
        .fetch_all(&self.pool)
        .await
        .context("bulk remove_tags")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("postgres ping")?;
        Ok(())
    }
}
