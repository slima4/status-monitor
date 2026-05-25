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
use crate::error::{AppError, Result};
use crate::security::Cipher;
use crate::storage::locks::{advisory_xact_lock, org_lock_key};
use crate::storage::postgres_secrets::{decrypt_in_place, encrypt_in_place};
use crate::storage::traits::{TargetFilter, TargetSort, TargetStore};

/// Org-scoped Postgres-backed target store. Every query binds the `org`
/// passed by the caller (resolved from the request's `CurrentOrg`) so reads,
/// updates, and deletes are isolated to that organisation. The store holds no
/// ambient org of its own — a missing scope is a compile error, not a silent
/// cross-tenant read. Cross-tenant operations (e.g. scheduler-wide
/// enumeration) go through `crate::storage::admin::AdminRepo`, never through
/// this type.
pub struct PostgresTargetStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
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
        assert_default_plan_present(&pool).await?;
        tracing::info!("postgres ready");
        Ok(pool)
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
        decode_target_row(row, self.cipher.as_deref())
    }

    fn rows_to_targets(&self, rows: Vec<TargetRow>) -> Result<Vec<Target>> {
        rows.into_iter().map(|r| self.decode_row(r)).collect()
    }
}

/// Refuse to boot if the literal `'free'` default referenced by
/// `organizations.plan_id` has no matching row. Plan-id renames are blocked
/// at the schema layer (BEFORE-UPDATE trigger on `plans`), so an absent row
/// can only come from an operator-side `DELETE`.
async fn assert_default_plan_present(pool: &PgPool) -> Result<()> {
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM plans WHERE id = 'free')")
            .fetch_one(pool)
            .await
            .context("assert_default_plan_present: query")?;
    if !exists {
        return Err(AppError::Other(anyhow::anyhow!(
            "plans.id = 'free' is missing — the literal `organizations.plan_id DEFAULT 'free'` would FK-violate on the next signup. Restore the row from the plans seed."
        )));
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
pub(crate) struct TargetRow {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) check_spec: serde_json::Value,
    pub(crate) interval_secs: i32,
    pub(crate) enabled: bool,
    pub(crate) tags: Vec<String>,
    pub(crate) alerts: serde_json::Value,
    pub(crate) group_name: Option<String>,
    pub(crate) owner_user_id: Option<Uuid>,
    pub(crate) public_status: bool,
    pub(crate) public_name: Option<String>,
    pub(crate) public_description: Option<String>,
    pub(crate) public_group: Option<String>,
    pub(crate) public_sort_order: i32,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

/// Build a `Target` from a row, decrypting `check_spec` if a cipher is
/// configured. Shared between [`PostgresTargetStore`] (org-scoped reads) and
/// [`crate::storage::AdminRepo`] (cross-tenant scheduler enumeration).
pub(crate) fn decode_target_row(row: TargetRow, cipher: Option<&Cipher>) -> Result<Target> {
    let mut check_json = row.check_spec;
    if let Some(cipher) = cipher {
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
        group_name: row.group_name,
        owner_user_id: row.owner_user_id,
        public_status: row.public_status,
        public_name: row.public_name,
        public_description: row.public_description,
        public_group: row.public_group,
        public_sort_order: row.public_sort_order,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

#[async_trait]
impl TargetStore for PostgresTargetStore {
    async fn list(&self, org: OrgId, filter: TargetFilter) -> Result<Vec<Target>> {
        let limit = filter.limit.unwrap_or(100).min(10_000) as i64;
        let offset = filter.offset as i64;
        let order_clause = match filter.sort {
            TargetSort::RecentActivity => "ORDER BY updated_at DESC",
            TargetSort::Name => "ORDER BY name ASC",
            TargetSort::Created => "ORDER BY created_at ASC",
        };
        let q_like = filter
            .q
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{s}%"));
        let group = filter
            .group
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let sql = format!(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      group_name, owner_user_id,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
               FROM targets
               WHERE org_id = $1
                 AND ($2::bool IS NULL OR enabled = $2)
                 AND ($3::text IS NULL OR $3 = ANY(tags))
                 AND ($4::text IS NULL OR name ILIKE $4)
                 AND ($5::text IS NULL OR group_name = $5)
                 AND ($6::uuid IS NULL OR owner_user_id = $6)
               {order_clause}
               LIMIT $7 OFFSET $8"#,
        );
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(&sql)
            .bind(org.0)
            .bind(filter.enabled)
            .bind(filter.tag)
            .bind(q_like)
            .bind(group)
            .bind(filter.owner)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .context("query targets")?;
        self.rows_to_targets(rows)
    }

    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<Target>> {
        let row: Option<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      group_name, owner_user_id,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
               FROM targets WHERE id = $1 AND org_id = $2"#,
        )
        .bind(id)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .context("get target by id")?;
        match row {
            Some(r) => Ok(Some(self.decode_row(r)?)),
            None => Ok(None),
        }
    }

    async fn create(&self, org: OrgId, new: NewTarget, max_targets: i64) -> Result<Target> {
        let check_json = self.encode_check(&new.check)?;
        let alerts_json = serde_json::to_value(&new.alerts).context("encoding alerts JSON")?;
        // A per-org advisory lock held across count+INSERT in one tx. The
        // count-in-INSERT predicate alone is NOT race-safe under READ
        // COMMITTED — concurrent creators each see a snapshot count and all
        // pass `+1 <= limit`, overshooting. This mirrors the owner-org and
        // invitation caps: lock the subject, then count, then write.
        let mut tx = self.pool.begin().await.context("create target: begin")?;
        advisory_xact_lock(&mut *tx, &org_lock_key(org))
            .await
            .context("create target: advisory lock")?;
        let (current,): (i64,) = sqlx::query_as("SELECT count(*) FROM targets WHERE org_id = $1")
            .bind(org.0)
            .fetch_one(&mut *tx)
            .await
            .context("create target: count")?;
        if current + 1 > max_targets {
            tx.rollback().await.ok();
            return Err(AppError::quota_exceeded(
                "max_targets",
                current,
                max_targets,
                "free",
            ));
        }
        let row: TargetRow = sqlx::query_as::<_, TargetRow>(
            r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled, tags, alerts,
                                    group_name, owner_user_id,
                                    public_status, public_name, public_description, public_group, public_sort_order)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
               RETURNING id, name, check_spec, interval_secs, enabled, tags, alerts,
                      group_name, owner_user_id,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at"#,
        )
        .bind(org.0)
        .bind(&new.name)
        .bind(check_json)
        .bind(new.interval.as_secs() as i32)
        .bind(new.enabled)
        .bind(&new.tags)
        .bind(alerts_json)
        .bind(&new.group_name)
        .bind(new.owner_user_id)
        .bind(new.public_status)
        .bind(&new.public_name)
        .bind(&new.public_description)
        .bind(&new.public_group)
        .bind(new.public_sort_order)
        .fetch_one(&mut *tx)
        .await
        .context("insert target")?;
        tx.commit().await.context("create target: commit")?;
        self.decode_row(row)
    }

    async fn update(&self, org: OrgId, id: Uuid, update: TargetUpdate) -> Result<Option<Target>> {
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
                 group_name = CASE WHEN $8::bool THEN $9 ELSE group_name END,
                 owner_user_id = CASE WHEN $10::bool THEN $11 ELSE owner_user_id END,
                 public_status = COALESCE($12, public_status),
                 public_name = CASE WHEN $13::bool THEN $14 ELSE public_name END,
                 public_description = CASE WHEN $15::bool THEN $16 ELSE public_description END,
                 public_group = CASE WHEN $17::bool THEN $18 ELSE public_group END,
                 public_sort_order = COALESCE($19, public_sort_order),
                 updated_at = now()
               WHERE id = $1 AND org_id = $20
               RETURNING id, name, check_spec, interval_secs, enabled, tags, alerts,
                      group_name, owner_user_id,
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
        .bind(update.group_name.is_some())
        .bind(update.group_name.clone().flatten())
        .bind(update.owner_user_id.is_some())
        .bind(update.owner_user_id.flatten())
        .bind(update.public_status)
        .bind(update.public_name.is_some())
        .bind(update.public_name.clone().flatten())
        .bind(update.public_description.is_some())
        .bind(update.public_description.clone().flatten())
        .bind(update.public_group.is_some())
        .bind(update.public_group.clone().flatten())
        .bind(update.public_sort_order)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .context("update target")?;
        match row {
            Some(r) => Ok(Some(self.decode_row(r)?)),
            None => Ok(None),
        }
    }

    async fn delete(&self, org: OrgId, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM targets WHERE id = $1 AND org_id = $2")
            .bind(id)
            .bind(org.0)
            .execute(&self.pool)
            .await
            .context("delete target")?;
        Ok(res.rows_affected() > 0)
    }

    async fn bulk_create(
        &self,
        org: OrgId,
        items: Vec<NewTarget>,
        max_targets: i64,
    ) -> Result<Vec<Target>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        // Same lock-then-count-then-write pattern as the singular path:
        // the per-org advisory lock makes the count accurate against a
        // concurrent bulk (a count subquery alone is not race-safe under
        // READ COMMITTED). All-or-nothing on the cap.
        const SQL: &str = r#"INSERT INTO targets (org_id, name, check_spec, interval_secs, enabled, tags, alerts,
                                    group_name, owner_user_id,
                                    public_status, public_name, public_description, public_group, public_sort_order)
               SELECT $14, u.name, u.check_spec, u.interval_secs, u.enabled,
                      ARRAY(SELECT jsonb_array_elements_text(u.tags)),
                      u.alerts,
                      u.group_name, u.owner_user_id,
                      u.public_status, u.public_name, u.public_description, u.public_group, u.public_sort_order
               FROM UNNEST($1::text[], $2::jsonb[], $3::int4[], $4::bool[], $5::jsonb[], $6::jsonb[],
                           $7::text[], $8::uuid[],
                           $9::bool[], $10::text[], $11::text[], $12::text[], $13::int4[])
                    AS u(name, check_spec, interval_secs, enabled, tags, alerts,
                         group_name, owner_user_id,
                         public_status, public_name, public_description, public_group, public_sort_order)
               RETURNING id, name, check_spec, interval_secs, enabled, tags, alerts,
                      group_name, owner_user_id,
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
        let mut group_name: Vec<Option<String>> = Vec::with_capacity(len);
        let mut owner_user_id: Vec<Option<Uuid>> = Vec::with_capacity(len);
        let mut public_status: Vec<bool> = Vec::with_capacity(len);
        let mut public_name: Vec<Option<String>> = Vec::with_capacity(len);
        let mut public_description: Vec<Option<String>> = Vec::with_capacity(len);
        let mut public_group: Vec<Option<String>> = Vec::with_capacity(len);
        let mut public_sort_order: Vec<i32> = Vec::with_capacity(len);

        let mut tx = self.pool.begin().await.context("bulk create: begin")?;
        advisory_xact_lock(&mut *tx, &org_lock_key(org))
            .await
            .context("bulk create: advisory lock")?;
        let (current,): (i64,) = sqlx::query_as("SELECT count(*) FROM targets WHERE org_id = $1")
            .bind(org.0)
            .fetch_one(&mut *tx)
            .await
            .context("bulk create: count")?;
        if current + len as i64 > max_targets {
            tx.rollback().await.ok();
            return Err(AppError::quota_exceeded(
                "max_targets",
                current,
                max_targets,
                "free",
            ));
        }

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
                group_name.push(new.group_name);
                owner_user_id.push(new.owner_user_id);
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
                .bind(&group_name)
                .bind(&owner_user_id)
                .bind(&public_status)
                .bind(&public_name)
                .bind(&public_description)
                .bind(&public_group)
                .bind(&public_sort_order)
                .bind(org.0)
                .fetch_all(&mut *tx)
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
                group_name.push(new.group_name);
                owner_user_id.push(new.owner_user_id);
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
                .bind(&group_name)
                .bind(&owner_user_id)
                .bind(&public_status)
                .bind(&public_name)
                .bind(&public_description)
                .bind(&public_group)
                .bind(&public_sort_order)
                .bind(org.0)
                .fetch_all(&mut *tx)
                .await
                .context("bulk insert targets")?
        };

        tx.commit().await.context("bulk create: commit")?;
        self.rows_to_targets(rows)
    }

    async fn list_updated_since(&self, org: OrgId, since: DateTime<Utc>) -> Result<Vec<Target>> {
        let rows: Vec<TargetRow> = sqlx::query_as::<_, TargetRow>(
            r#"SELECT id, name, check_spec, interval_secs, enabled, tags, alerts,
                      group_name, owner_user_id,
                      public_status, public_name, public_description, public_group, public_sort_order,
                      created_at, updated_at
               FROM targets WHERE org_id = $1 AND updated_at > $2"#,
        )
        .bind(org.0)
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("query targets updated since")?;
        self.rows_to_targets(rows)
    }

    async fn list_tags(
        &self,
        org: OrgId,
        prefix: Option<String>,
        limit: usize,
    ) -> Result<Vec<TagCount>> {
        let prefix_pat = prefix.as_deref().map(|p| format!("{p}%"));
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"SELECT tag, count(*) AS c
               FROM targets, unnest(tags) AS tag
               WHERE org_id = $1
                 AND ($2::text IS NULL OR tag LIKE $2)
               GROUP BY tag
               ORDER BY c DESC, tag ASC
               LIMIT $3"#,
        )
        .bind(org.0)
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

    async fn summary(&self, org: OrgId) -> Result<TargetsSummary> {
        let row: (i64, i64) = sqlx::query_as(
            r#"SELECT count(*) FILTER (WHERE TRUE),
                      count(*) FILTER (WHERE enabled)
               FROM targets
               WHERE org_id = $1"#,
        )
        .bind(org.0)
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

    async fn set_enabled(&self, org: OrgId, ids: &[Uuid], enabled: bool) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"UPDATE targets SET enabled = $2, updated_at = now()
               WHERE id = ANY($1) AND org_id = $3 RETURNING id"#,
        )
        .bind(ids)
        .bind(enabled)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .context("bulk set_enabled")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn delete_bulk(&self, org: OrgId, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"DELETE FROM targets WHERE id = ANY($1) AND org_id = $2 RETURNING id"#,
        )
        .bind(ids)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .context("bulk delete")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn add_tags(&self, org: OrgId, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>> {
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
               WHERE id = ANY($1) AND org_id = $3
               RETURNING id"#,
        )
        .bind(ids)
        .bind(tags)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .context("bulk add_tags")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn remove_tags(&self, org: OrgId, ids: &[Uuid], tags: &[String]) -> Result<Vec<Uuid>> {
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
               WHERE id = ANY($1) AND org_id = $3
               RETURNING id"#,
        )
        .bind(ids)
        .bind(tags)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .context("bulk remove_tags")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn set_group(&self, org: OrgId, ids: &[Uuid], group: Option<&str>) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"UPDATE targets SET group_name = $2, updated_at = now()
               WHERE id = ANY($1) AND org_id = $3 RETURNING id"#,
        )
        .bind(ids)
        .bind(group)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .context("bulk set_group")?;
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
