//! Storage for `org_variables` — reusable named values per org. A secret
//! variable's value is sealed with the KEK (plaintext fallback when no KEK,
//! same discipline as `monitor_shares` tokens and target credentials) and is
//! opened only in [`VariableStore::resolve_map`], which the worker calls to
//! interpolate `{{key}}` references at probe time. The operator-facing
//! [`Variable`] never carries a secret value.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::domain::{NewVariable, OrgId, ResolvedVar, UserId, VarMap, Variable, VariableId};
use crate::error::{AppError, Result};
use crate::security::{Cipher, open_str, seal_str};
use crate::storage::orgs::record_audit_tx;

/// Result of [`VariableStore::create`]. The store stays free of HTTP concerns;
/// the handler maps the conflict to a 409.
#[derive(Debug)]
pub enum CreateVariableOutcome {
    Created(Variable),
    /// An active variable with this key already exists in the org.
    DuplicateKey,
}

/// Seal a value for storage: sealed for a secret, plaintext for a plain variable.
fn seal_value(raw: &str, is_secret: bool, cipher: Option<&Cipher>) -> Result<String> {
    if !is_secret {
        return Ok(raw.to_string());
    }
    seal_str(raw, cipher)
        .map_err(|e| AppError::Other(anyhow::anyhow!("variable encryption failed: {e}")))
}

/// Org-scoped variable repository. Every method takes the caller's `org` first
/// so a missing scope is a compile error, never a cross-tenant read (mirrors
/// [`TargetStore`](crate::storage::TargetStore)).
#[async_trait]
pub trait VariableStore: Send + Sync {
    /// Operator view, secret values redacted, ordered by key.
    async fn list(&self, org: OrgId) -> Result<Vec<Variable>>;
    async fn get(&self, org: OrgId, id: VariableId) -> Result<Option<Variable>>;
    async fn create(
        &self,
        org: OrgId,
        new: NewVariable,
        actor: Option<UserId>,
    ) -> Result<CreateVariableOutcome>;
    /// Replace a variable's value, resealing if it is a secret. `None` when the
    /// id is absent from this org. The secret flag is fixed at create.
    async fn update_value(
        &self,
        org: OrgId,
        id: VariableId,
        new_value: &str,
        actor: Option<UserId>,
    ) -> Result<Option<Variable>>;
    async fn delete(&self, org: OrgId, id: VariableId, actor: Option<UserId>) -> Result<bool>;
    /// Decrypted `key -> value` map for the worker. Secrets that cannot be
    /// opened are omitted (treated as missing downstream).
    async fn resolve_map(&self, org: OrgId) -> Result<VarMap>;
    /// `key -> count of org monitors whose check_spec references {{key}}`. Powers
    /// the blast-radius display and the delete-block. Every key is present, with
    /// a zero count when unreferenced.
    async fn usage_counts(&self, org: OrgId) -> Result<std::collections::HashMap<String, i64>>;
    /// Reference count for one key, scanning monitors once instead of building
    /// the whole-org map for a single answer (get / rotate / delete).
    async fn usage_count(&self, org: OrgId, key: &str) -> Result<i64>;
}

pub struct PgVariableStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
}

impl PgVariableStore {
    pub fn new(pool: PgPool, cipher: Option<Arc<Cipher>>) -> Self {
        Self { pool, cipher }
    }
}

#[derive(sqlx::FromRow)]
struct VarRow {
    id: uuid::Uuid,
    org_id: uuid::Uuid,
    key: String,
    is_secret: bool,
    value: String,
    updated_at: DateTime<Utc>,
    updated_by: Option<uuid::Uuid>,
}

impl VarRow {
    /// Operator view: a plain value passes through, a secret is redacted here so
    /// the sealed bytes never reach a read surface.
    fn into_view(self) -> Variable {
        let value = (!self.is_secret).then_some(self.value);
        Variable {
            id: VariableId(self.id),
            org_id: OrgId(self.org_id),
            key: self.key,
            is_secret: self.is_secret,
            value,
            updated_at: self.updated_at,
            updated_by: self.updated_by.map(UserId),
        }
    }
}

const VAR_COLUMNS: &str = "id, org_id, key, is_secret, value, updated_at, updated_by";

#[async_trait]
impl VariableStore for PgVariableStore {
    async fn list(&self, org: OrgId) -> Result<Vec<Variable>> {
        let rows: Vec<VarRow> = sqlx::query_as(&format!(
            "SELECT {VAR_COLUMNS} FROM org_variables WHERE org_id = $1 ORDER BY key"
        ))
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(VarRow::into_view).collect())
    }

    async fn get(&self, org: OrgId, id: VariableId) -> Result<Option<Variable>> {
        let row: Option<VarRow> = sqlx::query_as(&format!(
            "SELECT {VAR_COLUMNS} FROM org_variables WHERE org_id = $1 AND id = $2"
        ))
        .bind(org.0)
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(VarRow::into_view))
    }

    async fn create(
        &self,
        org: OrgId,
        new: NewVariable,
        actor: Option<UserId>,
    ) -> Result<CreateVariableOutcome> {
        let stored = seal_value(&new.value, new.is_secret, self.cipher.as_deref())?;
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let row: Option<VarRow> = sqlx::query_as(&format!(
            r#"INSERT INTO org_variables (org_id, key, is_secret, value, updated_by)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (org_id, key) DO NOTHING
               RETURNING {VAR_COLUMNS}"#
        ))
        .bind(org.0)
        .bind(&new.key)
        .bind(new.is_secret)
        .bind(&stored)
        .bind(actor.map(|u| u.0))
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let Some(row) = row else {
            tx.rollback().await.ok();
            return Ok(CreateVariableOutcome::DuplicateKey);
        };
        record_audit_tx(
            &mut tx,
            org,
            actor,
            "variable.created",
            serde_json::json!({ "variable_id": row.id, "key": row.key, "is_secret": row.is_secret }),
        )
        .await?;
        tx.commit().await.map_err(db_err)?;
        Ok(CreateVariableOutcome::Created(row.into_view()))
    }

    async fn update_value(
        &self,
        org: OrgId,
        id: VariableId,
        new_value: &str,
        actor: Option<UserId>,
    ) -> Result<Option<Variable>> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let existing: Option<(bool,)> = sqlx::query_as(
            "SELECT is_secret FROM org_variables WHERE org_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(org.0)
        .bind(id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        let Some((is_secret,)) = existing else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        let stored = seal_value(new_value, is_secret, self.cipher.as_deref())?;
        let row: VarRow = sqlx::query_as(&format!(
            r#"UPDATE org_variables SET value = $3, updated_at = now(), updated_by = $4
               WHERE org_id = $1 AND id = $2
               RETURNING {VAR_COLUMNS}"#
        ))
        .bind(org.0)
        .bind(id.0)
        .bind(&stored)
        .bind(actor.map(|u| u.0))
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        record_audit_tx(
            &mut tx,
            org,
            actor,
            "variable.updated",
            serde_json::json!({ "variable_id": row.id, "key": row.key }),
        )
        .await?;
        tx.commit().await.map_err(db_err)?;
        Ok(Some(row.into_view()))
    }

    async fn delete(&self, org: OrgId, id: VariableId, actor: Option<UserId>) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let deleted: Option<(uuid::Uuid, String)> = sqlx::query_as(
            "DELETE FROM org_variables WHERE org_id = $1 AND id = $2 RETURNING id, key",
        )
        .bind(org.0)
        .bind(id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if let Some((_, key)) = &deleted {
            record_audit_tx(
                &mut tx,
                org,
                actor,
                "variable.deleted",
                serde_json::json!({ "variable_id": id.0, "key": key }),
            )
            .await?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(deleted.is_some())
    }

    async fn resolve_map(&self, org: OrgId) -> Result<VarMap> {
        let rows: Vec<VarRow> = sqlx::query_as(&format!(
            "SELECT {VAR_COLUMNS} FROM org_variables WHERE org_id = $1"
        ))
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let cipher = self.cipher.as_deref();
        let mut map = VarMap::with_capacity(rows.len());
        for r in rows {
            let is_secret = r.is_secret;
            let value = if is_secret {
                match open_str(&r.value, cipher) {
                    Some(v) => v,
                    None => continue,
                }
            } else {
                r.value
            };
            map.insert(r.key, ResolvedVar { value, is_secret });
        }
        Ok(map)
    }

    async fn usage_counts(&self, org: OrgId) -> Result<std::collections::HashMap<String, i64>> {
        // Match both the literal `{{ key }}` token and its URL-encoded path form
        // `%7B%7Bkey%7D%7D`, so the delete-block never frees a referenced variable.
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"SELECT v.key, (
                   SELECT count(*) FROM targets t
                   WHERE t.org_id = $1
                     AND (t.check_spec::text ~ ('\{\{[[:space:]]*' || v.key || '[[:space:]]*\}\}')
                          OR position('%7B%7B' || v.key || '%7D%7D' in t.check_spec::text) > 0)
               )::bigint
               FROM org_variables v WHERE v.org_id = $1"#,
        )
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().collect())
    }

    async fn usage_count(&self, org: OrgId, key: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            r#"SELECT count(*)::bigint FROM targets t
               WHERE t.org_id = $1
                 AND (t.check_spec::text ~ ('\{\{[[:space:]]*' || $2 || '[[:space:]]*\}\}')
                      OR position('%7B%7B' || $2 || '%7D%7D' in t.check_spec::text) > 0)"#,
        )
        .bind(org.0)
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(count)
    }
}

fn db_err(e: sqlx::Error) -> AppError {
    AppError::Other(anyhow::anyhow!("org_variables: {e}"))
}

// ── In-memory store (no-DB harnesses) ─────────────────────────────────────────

struct MemVar {
    var: Variable,
    /// Raw value retained for `resolve_map`; the operator view in `var` redacts
    /// secrets.
    raw: String,
}

/// In-memory [`VariableStore`] mirroring [`PgVariableStore`] semantics, minus
/// the at-rest sealing (it holds raw values). Used by no-DB fixtures.
#[derive(Default)]
pub struct InMemoryVariableStore {
    inner: std::sync::Mutex<Vec<MemVar>>,
}

impl InMemoryVariableStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VariableStore for InMemoryVariableStore {
    async fn list(&self, org: OrgId) -> Result<Vec<Variable>> {
        let st = self.inner.lock().unwrap();
        let mut out: Vec<Variable> = st
            .iter()
            .filter(|m| m.var.org_id == org)
            .map(|m| m.var.clone())
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn get(&self, org: OrgId, id: VariableId) -> Result<Option<Variable>> {
        let st = self.inner.lock().unwrap();
        Ok(st
            .iter()
            .find(|m| m.var.org_id == org && m.var.id == id)
            .map(|m| m.var.clone()))
    }

    async fn create(
        &self,
        org: OrgId,
        new: NewVariable,
        actor: Option<UserId>,
    ) -> Result<CreateVariableOutcome> {
        let mut st = self.inner.lock().unwrap();
        if st
            .iter()
            .any(|m| m.var.org_id == org && m.var.key == new.key)
        {
            return Ok(CreateVariableOutcome::DuplicateKey);
        }
        let var = Variable {
            id: VariableId(uuid::Uuid::new_v4()),
            org_id: org,
            key: new.key,
            is_secret: new.is_secret,
            value: (!new.is_secret).then(|| new.value.clone()),
            updated_at: Utc::now(),
            updated_by: actor,
        };
        let view = var.clone();
        st.push(MemVar {
            var,
            raw: new.value,
        });
        Ok(CreateVariableOutcome::Created(view))
    }

    async fn update_value(
        &self,
        org: OrgId,
        id: VariableId,
        new_value: &str,
        actor: Option<UserId>,
    ) -> Result<Option<Variable>> {
        let mut st = self.inner.lock().unwrap();
        let Some(m) = st
            .iter_mut()
            .find(|m| m.var.org_id == org && m.var.id == id)
        else {
            return Ok(None);
        };
        m.raw = new_value.to_string();
        m.var.value = (!m.var.is_secret).then(|| new_value.to_string());
        m.var.updated_at = Utc::now();
        m.var.updated_by = actor;
        Ok(Some(m.var.clone()))
    }

    async fn delete(&self, org: OrgId, id: VariableId, _actor: Option<UserId>) -> Result<bool> {
        let mut st = self.inner.lock().unwrap();
        let before = st.len();
        st.retain(|m| !(m.var.org_id == org && m.var.id == id));
        Ok(st.len() != before)
    }

    async fn resolve_map(&self, org: OrgId) -> Result<VarMap> {
        let st = self.inner.lock().unwrap();
        Ok(st
            .iter()
            .filter(|m| m.var.org_id == org)
            .map(|m| {
                (
                    m.var.key.clone(),
                    ResolvedVar {
                        value: m.raw.clone(),
                        is_secret: m.var.is_secret,
                    },
                )
            })
            .collect())
    }

    /// No targets in the no-DB fixture, so every count is zero. The PG store
    /// owns the real reference scan; delete-block behaviour is tested there.
    async fn usage_counts(&self, org: OrgId) -> Result<std::collections::HashMap<String, i64>> {
        let st = self.inner.lock().unwrap();
        Ok(st
            .iter()
            .filter(|m| m.var.org_id == org)
            .map(|m| (m.var.key.clone(), 0))
            .collect())
    }

    async fn usage_count(&self, _org: OrgId, _key: &str) -> Result<i64> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{Cipher, is_envelope};

    fn org(n: u128) -> OrgId {
        OrgId(uuid::Uuid::from_u128(n))
    }

    fn plain(key: &str, value: &str) -> NewVariable {
        NewVariable {
            key: key.into(),
            is_secret: false,
            value: value.into(),
        }
    }

    fn secret(key: &str, value: &str) -> NewVariable {
        NewVariable {
            key: key.into(),
            is_secret: true,
            value: value.into(),
        }
    }

    fn created(o: CreateVariableOutcome) -> Variable {
        match o {
            CreateVariableOutcome::Created(v) => v,
            CreateVariableOutcome::DuplicateKey => panic!("expected Created, got DuplicateKey"),
        }
    }

    #[tokio::test]
    async fn create_list_get_roundtrip() {
        let store = InMemoryVariableStore::new();
        let v = created(
            store
                .create(org(1), plain("base_url", "https://x"), None)
                .await
                .unwrap(),
        );
        assert_eq!(store.list(org(1)).await.unwrap().len(), 1);
        let got = store.get(org(1), v.id).await.unwrap().unwrap();
        assert_eq!(got.value.as_deref(), Some("https://x"));
    }

    #[tokio::test]
    async fn duplicate_key_rejected() {
        let store = InMemoryVariableStore::new();
        store.create(org(1), plain("k", "a"), None).await.unwrap();
        assert!(matches!(
            store.create(org(1), plain("k", "b"), None).await.unwrap(),
            CreateVariableOutcome::DuplicateKey
        ));
        // Same key, different org is allowed.
        assert!(matches!(
            store.create(org(2), plain("k", "b"), None).await.unwrap(),
            CreateVariableOutcome::Created(_)
        ));
    }

    #[tokio::test]
    async fn secret_value_redacted_in_views_but_resolves() {
        let store = InMemoryVariableStore::new();
        let v = created(
            store
                .create(org(1), secret("api_key", "sk-123"), None)
                .await
                .unwrap(),
        );
        assert!(v.value.is_none());
        assert!(
            store
                .get(org(1), v.id)
                .await
                .unwrap()
                .unwrap()
                .value
                .is_none()
        );
        let map = store.resolve_map(org(1)).await.unwrap();
        assert_eq!(map["api_key"].value, "sk-123");
        assert!(map["api_key"].is_secret);
    }

    #[tokio::test]
    async fn update_value_rotates() {
        let store = InMemoryVariableStore::new();
        let v = created(
            store
                .create(org(1), secret("api_key", "old"), None)
                .await
                .unwrap(),
        );
        store.update_value(org(1), v.id, "new", None).await.unwrap();
        assert_eq!(
            store.resolve_map(org(1)).await.unwrap()["api_key"].value,
            "new"
        );
    }

    #[tokio::test]
    async fn tenant_isolation_on_every_method() {
        let store = InMemoryVariableStore::new();
        let v = created(store.create(org(1), plain("k", "a"), None).await.unwrap());
        assert!(store.get(org(2), v.id).await.unwrap().is_none());
        assert!(
            store
                .update_value(org(2), v.id, "x", None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!store.delete(org(2), v.id, None).await.unwrap());
        assert!(store.resolve_map(org(2)).await.unwrap().is_empty());
        // Original survived the cross-tenant attempts.
        assert_eq!(
            store
                .get(org(1), v.id)
                .await
                .unwrap()
                .unwrap()
                .value
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn seal_open_secret_roundtrip_with_kek() {
        let cipher = Cipher::from_base64(&base64_kek()).unwrap();
        let sealed = seal_value("sk-123", true, Some(&cipher)).unwrap();
        assert!(is_envelope(&sealed));
        assert_eq!(open_str(&sealed, Some(&cipher)).as_deref(), Some("sk-123"));
    }

    #[test]
    fn plaintext_fallback_without_kek() {
        let stored = seal_value("sk-123", true, None).unwrap();
        assert_eq!(stored, "sk-123");
        assert!(!is_envelope(&stored));
        assert_eq!(open_str(&stored, None).as_deref(), Some("sk-123"));
    }

    #[test]
    fn plain_value_never_sealed() {
        let cipher = Cipher::from_base64(&base64_kek()).unwrap();
        let stored = seal_value("https://x", false, Some(&cipher)).unwrap();
        assert_eq!(stored, "https://x");
    }

    #[test]
    fn sealed_secret_unopenable_without_kek() {
        let cipher = Cipher::from_base64(&base64_kek()).unwrap();
        let sealed = seal_value("sk-123", true, Some(&cipher)).unwrap();
        assert!(open_str(&sealed, None).is_none());
    }

    fn base64_kek() -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32])
    }
}
