//! Cross-tenant data access. The only escape hatch out of the org-scoped
//! repositories. Every constructor records a static `reason` so an audit grep
//! over the codebase enumerates every cross-tenant entry point.
//!
//! Rules (also enforced by `scripts/sg-rules/admin_repo_static_reason.yml`):
//!  * `AdminRepo::new` requires `&'static str` — dynamic strings would defeat
//!    the audit, since a future caller could format an attacker-controlled
//!    value into the reason field.
//!  * Methods on this type intentionally do **not** filter by `org_id`. The
//!    type itself is the audit signal; readers see `AdminRepo` and know the
//!    query crosses tenant boundaries on purpose.
//!  * No method here returns per-org user-facing data. Only aggregates and
//!    background-job inputs (scheduler enumeration, purge queue, etc.).
//!
//! Audit caveat: the grep `rg 'AdminRepo::new\('` enumerates direct call
//! sites only. A `use AdminRepo as Repo` (or trait-impl rename via macros)
//! would dodge it. Code review must reject such renames; the ast-grep rule
//! has the same blind spot.

use anyhow::Context;
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::{OrgId, Target};
use crate::error::Result;
use crate::security::Cipher;
use crate::storage::postgres::{TargetRow, decode_target_row};

/// Source of every enabled target across every organisation, for the
/// scheduler's global registry. Each target is paired with its owning
/// [`OrgId`] so the scheduler→worker→alert path can resolve channels
/// tenant-scoped (a target only ever reaches its own org's channels).
/// Implemented by [`AdminRepo`] (production) and by
/// [`crate::storage::InMemoryTargetStore`] (single-org test fixtures).
#[async_trait]
pub trait EnabledTargetSource: Send + Sync {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>>;
}

#[async_trait]
impl EnabledTargetSource for AdminRepo {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        AdminRepo::list_all_enabled_targets(self).await
    }
}

/// Scheduler source scoped to the control plane's own region. Wraps
/// [`AdminRepo`] so the local scheduler runs exactly the targets assigned to
/// its region — the same query an agent pulls for its region. Remote regions
/// are left to their agents.
pub struct RegionTargetSource {
    repo: AdminRepo,
    region: String,
}

impl RegionTargetSource {
    pub fn new(repo: AdminRepo, region: String) -> Self {
        Self { repo, region }
    }
}

#[async_trait]
impl EnabledTargetSource for RegionTargetSource {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        self.repo
            .list_enabled_targets_for_region(&self.region)
            .await
    }
}

/// Keyset cursor over `(org_id, target_id)` ascending. `None` means "start
/// from the beginning"; subsequent pages pass the last row's pair to skip
/// past it on the next read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicTargetCursor {
    pub org_id: OrgId,
    pub target_id: Uuid,
}

impl PublicTargetCursor {
    pub fn after(org_id: OrgId, target_id: Uuid) -> Self {
        Self { org_id, target_id }
    }
}

/// Paginated cross-tenant walk over every enabled target in *live*
/// organisations — the set the incident writer watches. Separate from
/// [`EnabledTargetSource`] because the access pattern differs: the scheduler
/// builds a single in-memory registry (full snapshot, infrequent), whereas the
/// incident writer streams every 30 s and must keep page-sized memory +
/// bounded SQL load at 10k+ orgs. Whether a target is also a public status-page
/// component is decided at incident-insert time, not here.
#[async_trait]
pub trait EnabledTargetStream: Send + Sync {
    /// Next page of `(org_id, target)` strictly after `after`, up to `limit`
    /// rows, ordered by `(org_id, target_id)` ascending. An empty result
    /// signals the walk is done.
    async fn next_enabled_target_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>>;
}

#[async_trait]
impl EnabledTargetStream for AdminRepo {
    async fn next_enabled_target_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>> {
        AdminRepo::next_enabled_target_page(self, after, limit).await
    }
}

/// `targets` row plus its `org_id`. Reuses [`TargetRow`] via `#[sqlx(flatten)]`
/// so the column list stays single-sourced with the org-scoped store.
#[derive(sqlx::FromRow)]
struct OrgTargetRow {
    org_id: Uuid,
    #[sqlx(flatten)]
    target: TargetRow,
}

/// Single source for the cross-tenant target column list. Both the
/// scheduler-snapshot and incident-writer-keyset queries return the same
/// `targets` shape that [`decode_target_row`] consumes.
const TARGET_COLUMNS: &str = "t.org_id, t.id, t.name, t.check_spec, t.interval_secs, t.enabled, t.tags, t.alerts, t.region_policy, t.alert_confirmations, t.notify_recovery, t.renotify_interval_secs, t.group_name, t.owner_user_id, t.write_source, t.created_at, t.updated_at";

/// Per-row lenient decode: skip (logged + counted) any row whose `check_spec`,
/// `alerts`, or credential decrypt won't parse, rather than failing the whole
/// batch — one bad row (a shape predating a schema change, a credential that
/// won't decrypt) must never blind the scheduler to every other target.
///
/// No all-failed guard here: a page of all-bad rows is not necessarily systemic
/// (bad rows cluster by the `(org_id, id)` keyset), so the paginator skips and
/// advances rather than aborting the walk. The snapshot reads layer the guard
/// on top via [`decode_targets_or_err`].
fn decode_targets_skipping(rows: Vec<OrgTargetRow>, cipher: Option<&Cipher>) -> Vec<(OrgId, Target)> {
    rows.into_iter()
        .filter_map(|r| {
            let org = OrgId(r.org_id);
            let target_id = r.target.id;
            match decode_target_row(r.target, cipher) {
                Ok(t) => Some((org, t)),
                Err(err) => {
                    tracing::error!(
                        %target_id,
                        org_id = %org.0,
                        error = ?err,
                        "skipping target that failed to decode (check_spec/alerts/credentials)"
                    );
                    metrics::counter!("uptimepage_undecodable_targets_total").increment(1);
                    None
                }
            }
        })
        .collect()
}

/// Snapshot decode for the scheduler's full lists. Skips one bad row, but if
/// *every* row fails it's a systemic fault (wrong KEK, schema drift): an empty
/// result here is dangerous — callers read it as "no targets" and drop the
/// whole fleet — so all-failed returns `Err`, keeping prior state and surfacing
/// loudly. A genuinely empty input returns `Ok(empty)` as normal.
fn decode_targets_or_err(
    rows: Vec<OrgTargetRow>,
    cipher: Option<&Cipher>,
) -> Result<Vec<(OrgId, Target)>> {
    let total = rows.len();
    let decoded = decode_targets_skipping(rows, cipher);
    if decoded.is_empty() && total > 0 {
        return Err(anyhow::anyhow!(
            "all {total} cross-tenant targets failed to decode — likely a cipher/KEK or schema fault"
        )
        .into());
    }
    Ok(decoded)
}

pub struct AdminRepo {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
    reason: &'static str,
}

impl AdminRepo {
    /// `reason` is recorded for audit. Use a short, lower-case identifier
    /// pinned to the call site (e.g. `"scheduler_refresh"`,
    /// `"purge_worker"`). The `&'static str` bound prevents dynamic strings
    /// that would defeat a CI grep.
    pub fn new(pool: PgPool, cipher: Option<Arc<Cipher>>, reason: &'static str) -> Self {
        tracing::debug!(reason, "AdminRepo constructed");
        Self {
            pool,
            cipher,
            reason,
        }
    }

    pub fn reason(&self) -> &'static str {
        self.reason
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// All enabled targets across every *live* organisation. The
    /// `organizations.deleted_at IS NULL` filter is load-bearing: a
    /// soft-deleted org has signalled "stop monitoring me" and the 30-day
    /// recovery grace window must not double as a 30-day stream of
    /// post-deletion check writes, alert deliveries, and ClickHouse rows
    /// the cascade purge then has to clear.
    pub async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        let sql = format!(
            "SELECT {TARGET_COLUMNS} \
             FROM targets t JOIN organizations o ON o.id = t.org_id \
             WHERE t.enabled = true AND o.deleted_at IS NULL"
        );
        let rows: Vec<OrgTargetRow> = sqlx::query_as::<_, OrgTargetRow>(&sql)
            .fetch_all(&self.pool)
            .await
            .context("admin: list all enabled targets")?;
        decode_targets_or_err(rows, self.cipher.as_deref())
    }

    /// Boot reconciliation for the config-driven region model. Idempotent.
    /// Upserts the control plane's own region and the new-target default region
    /// so their FK targets exist, then assigns every still-unassigned enabled
    /// target to the default region — so no target is ever orphaned between the
    /// region tables existing and the create path writing assignments.
    pub async fn reconcile_regions(
        &self,
        scheduler_region: &str,
        default_region: &str,
    ) -> Result<()> {
        for id in [scheduler_region, default_region] {
            sqlx::query(
                "INSERT INTO regions (id, name, location) VALUES ($1, $1, '') \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(id)
            .execute(&self.pool)
            .await
            .context("admin: reconcile_regions upsert region")?;
        }
        sqlx::query(
            "INSERT INTO target_regions (target_id, region) \
             SELECT t.id, $1 FROM targets t \
             WHERE NOT EXISTS (SELECT 1 FROM target_regions tr WHERE tr.target_id = t.id) \
             ON CONFLICT DO NOTHING",
        )
        .bind(default_region)
        .execute(&self.pool)
        .await
        .context("admin: reconcile_regions backfill assignments")?;
        Ok(())
    }

    /// Cheap config-pull validator for one region: a digest over the assigned
    /// targets' ids and a hash of each `check_spec`, plus count + max
    /// `updated_at`. The per-row `check_spec` hash means the etag changes on any
    /// content rewrite — including a credential re-encrypt (KEK rotation) that
    /// leaves `updated_at` untouched — so an agent never serves stale config off
    /// a `304`. Still no decrypt: it hashes the stored (encrypted) ciphertext.
    pub async fn region_pull_etag(&self, region: &str) -> Result<String> {
        let row: (i64, Option<chrono::DateTime<chrono::Utc>>, Option<String>) = sqlx::query_as(
            "SELECT count(*)::bigint, max(t.updated_at), \
                    md5(coalesce(string_agg(t.id::text || ':' || md5(t.check_spec::text), \
                                            ',' ORDER BY t.id), '')) \
             FROM target_regions tr \
             JOIN targets t ON t.id = tr.target_id \
             JOIN organizations o ON o.id = t.org_id \
             JOIN regions rg ON rg.id = tr.region \
             WHERE tr.region = $1 AND t.enabled = true AND o.deleted_at IS NULL \
               AND rg.enabled",
        )
        .bind(region)
        .fetch_one(&self.pool)
        .await
        .context("admin: region pull etag")?;
        let (count, max_updated, digest) = row;
        let ts = max_updated.map(|d| d.timestamp_millis()).unwrap_or(0);
        Ok(format!("\"{count}-{ts}-{}\"", digest.unwrap_or_default()))
    }

    /// Enabled targets assigned to one region (via `target_regions`), in live
    /// orgs. Backs the agent config-pull API. Same decrypted shape as
    /// [`Self::list_all_enabled_targets`].
    pub async fn list_enabled_targets_for_region(
        &self,
        region: &str,
    ) -> Result<Vec<(OrgId, Target)>> {
        let sql = format!(
            "SELECT {TARGET_COLUMNS} \
             FROM targets t \
             JOIN organizations o ON o.id = t.org_id \
             JOIN target_regions tr ON tr.target_id = t.id \
             JOIN regions rg ON rg.id = tr.region \
             WHERE t.enabled = true AND o.deleted_at IS NULL AND tr.region = $1 \
               AND rg.enabled"
        );
        let rows: Vec<OrgTargetRow> = sqlx::query_as::<_, OrgTargetRow>(&sql)
            .bind(region)
            .fetch_all(&self.pool)
            .await
            .context("admin: list enabled targets for region")?;
        decode_targets_or_err(rows, self.cipher.as_deref())
    }

    /// Map of enabled `target_id -> owning org` for one region. Ingest uses it
    /// both to reject results for targets outside the agent's region and to
    /// stamp the authoritative `org_id` (never trusting the agent-supplied one).
    pub async fn assigned_targets_for_region(
        &self,
        region: &str,
    ) -> Result<std::collections::HashMap<Uuid, OrgId>> {
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT tr.target_id, t.org_id \
             FROM target_regions tr \
             JOIN targets t ON t.id = tr.target_id \
             JOIN organizations o ON o.id = t.org_id \
             JOIN regions rg ON rg.id = tr.region \
             WHERE tr.region = $1 AND t.enabled = true AND o.deleted_at IS NULL \
               AND rg.enabled",
        )
        .bind(region)
        .fetch_all(&self.pool)
        .await
        .context("admin: assigned targets for region")?;
        Ok(rows.into_iter().map(|(t, o)| (t, OrgId(o))).collect())
    }

    /// Keyset-paginated walk over every enabled target in a live org — the set
    /// the incident writer watches. Incidents open for any monitor, not only
    /// public status-page components; whether the resulting incident is
    /// publicly visible is decided when it is inserted. Keyset-ordered by
    /// `(org_id, id)`; `limit` is clamped.
    pub async fn next_enabled_target_page(
        &self,
        after: Option<PublicTargetCursor>,
        limit: usize,
    ) -> Result<Vec<(OrgId, Target)>> {
        const MAX_PAGE: usize = 2_048;
        let limit = limit.clamp(1, MAX_PAGE) as i64;
        let base = format!(
            "SELECT {TARGET_COLUMNS} \
             FROM targets t JOIN organizations o ON o.id = t.org_id \
             WHERE t.enabled = true AND o.deleted_at IS NULL"
        );
        // Advance by the raw last row, not the last *decoded* one: if an entire
        // page decodes to nothing (a clustered run of bad rows), skip past it
        // and keep reading instead of returning empty — empty must mean "no more
        // rows", which is the caller's walk-complete signal.
        let mut cursor = after;
        loop {
            let rows: Vec<OrgTargetRow> = match cursor {
                None => {
                    let sql = format!("{base} ORDER BY t.org_id, t.id LIMIT $1");
                    sqlx::query_as::<_, OrgTargetRow>(&sql)
                        .bind(limit)
                        .fetch_all(&self.pool)
                        .await
                }
                Some(c) => {
                    let sql = format!(
                        "{base} AND (t.org_id, t.id) > ($1, $2) ORDER BY t.org_id, t.id LIMIT $3"
                    );
                    sqlx::query_as::<_, OrgTargetRow>(&sql)
                        .bind(c.org_id.0)
                        .bind(c.target_id)
                        .bind(limit)
                        .fetch_all(&self.pool)
                        .await
                }
            }
            .context("admin: next enabled-target page")?;
            let Some(raw_last) = rows.last() else {
                return Ok(Vec::new()); // no more rows — walk complete
            };
            let next = PublicTargetCursor::after(OrgId(raw_last.org_id), raw_last.target.id);
            let decoded = decode_targets_skipping(rows, self.cipher.as_deref());
            if !decoded.is_empty() {
                return Ok(decoded);
            }
            cursor = Some(next);
        }
    }
}
