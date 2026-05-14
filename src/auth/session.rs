//! DB-backed sessions: row CRUD, cookie helpers, debounced `last_used_at`
//! refresh.
//!
//! The session id is the cookie value: 32 OS-random bytes, base64url-no-pad
//! encoded (43 chars). The same id is the primary key in `sessions`, so a
//! lookup is one PK probe.
//!
//! `last_used_at` is updated lazily — the in-process `moka` cache remembers
//! when each session was last persisted and skips the UPDATE if `now()` is
//! within the debounce window. Without that, at 100 RPS one session generates
//! 100 writes per second; with the debounce it's at most 1 per minute (one per
//! replica in multi-replica deployments).

use std::time::Duration as StdDuration;

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use moka::sync::Cache;
use rand::TryRng;
use rand::rngs::SysRng;
use sqlx::PgPool;
use tower_cookies::cookie::{Cookie, SameSite};
use uuid::Uuid;

use crate::config::SessionConfig;
use crate::domain::{OrgId, UserId};
use crate::error::Result;

/// Debounce window for `last_used_at` writes — see module docs.
pub const LAST_USED_DEBOUNCE_SECS: u64 = 60;

/// Cap on how many session ids the debounce cache remembers across all
/// replicas of an `AppState`. 100k entries × ~70 bytes-each is bounded enough
/// to live next to the dashboard cache.
const DEBOUNCE_CACHE_CAPACITY: u64 = 100_000;

/// In-process "I already wrote `last_used_at` for this session recently" set,
/// keyed by session id. Cheap to clone — `moka::sync::Cache` is `Arc` inside.
pub type LastUsedDebounce = Cache<String, DateTime<Utc>>;

pub fn build_debounce_cache() -> LastUsedDebounce {
    Cache::builder()
        .max_capacity(DEBOUNCE_CACHE_CAPACITY)
        .time_to_live(StdDuration::from_secs(LAST_USED_DEBOUNCE_SECS * 4))
        .build()
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub user_id: UserId,
    pub active_org_id: Option<OrgId>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Outcome of a session lookup with timeout checks applied. The cookie path
/// uses this to decide whether to update `last_used_at` or clear the cookie.
#[derive(Debug)]
pub enum LookupOutcome {
    Active(SessionRow),
    Expired,
    Missing,
}

/// 32 cryptographically random bytes, base64url-no-pad. Matches the spec's
/// cookie format (43 ASCII chars, no padding to keep cookie value parsing
/// trivial).
pub fn generate_session_id() -> String {
    let mut bytes = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("SysRng must succeed for session id");
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// INSERT a fresh session row. `expires_at` is `now() + absolute_timeout_days`.
pub async fn create(
    pool: &PgPool,
    cfg: &SessionConfig,
    user: UserId,
    active_org_id: Option<OrgId>,
    ip_hash: Option<&str>,
    user_agent_hash: Option<&str>,
) -> Result<SessionRow> {
    let id = generate_session_id();
    let expires_at = Utc::now() + Duration::days(i64::from(cfg.absolute_timeout_days));
    let row: (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO sessions \
            (id, user_id, active_org_id, expires_at, ip_hash, user_agent_hash) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING last_used_at, expires_at",
    )
    .bind(&id)
    .bind(user.0)
    .bind(active_org_id.map(|o| o.0))
    .bind(expires_at)
    .bind(ip_hash)
    .bind(user_agent_hash)
    .fetch_one(pool)
    .await
    .context("session::create")?;
    Ok(SessionRow {
        id,
        user_id: user,
        active_org_id,
        last_used_at: row.0,
        expires_at: row.1,
    })
}

/// Look up a session and apply idle + absolute timeout. Expired rows are
/// deleted before returning so a leaked cookie cannot be reanimated by clock
/// drift on the way out.
type LookupRow = (Uuid, Option<Uuid>, DateTime<Utc>, DateTime<Utc>);

pub async fn lookup(pool: &PgPool, cfg: &SessionConfig, session_id: &str) -> Result<LookupOutcome> {
    let row: Option<LookupRow> = sqlx::query_as(
        "SELECT user_id, active_org_id, last_used_at, expires_at \
         FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .context("session::lookup")?;

    let Some((user_id, active_org_id, last_used_at, expires_at)) = row else {
        return Ok(LookupOutcome::Missing);
    };

    let now = Utc::now();
    let idle_limit = Duration::days(i64::from(cfg.idle_timeout_days));
    if expires_at <= now || now.signed_duration_since(last_used_at) > idle_limit {
        destroy(pool, session_id).await?;
        return Ok(LookupOutcome::Expired);
    }

    Ok(LookupOutcome::Active(SessionRow {
        id: session_id.to_string(),
        user_id: UserId(user_id),
        active_org_id: active_org_id.map(OrgId),
        last_used_at,
        expires_at,
    }))
}

/// Updates `last_used_at` to `now()` no more than once per
/// [`LAST_USED_DEBOUNCE_SECS`] per session per replica.
pub async fn touch_last_used_debounced(
    pool: &PgPool,
    cache: &LastUsedDebounce,
    session_id: &str,
) -> Result<()> {
    let now = Utc::now();
    if let Some(last) = cache.get(session_id)
        && now.signed_duration_since(last) < Duration::seconds(LAST_USED_DEBOUNCE_SECS as i64)
    {
        return Ok(());
    }
    sqlx::query("UPDATE sessions SET last_used_at = now() WHERE id = $1")
        .bind(session_id)
        .execute(pool)
        .await
        .context("session::touch_last_used_debounced")?;
    cache.insert(session_id.to_string(), now);
    Ok(())
}

pub async fn destroy(pool: &PgPool, session_id: &str) -> Result<u64> {
    let res = sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(session_id)
        .execute(pool)
        .await
        .context("session::destroy")?;
    Ok(res.rows_affected())
}

/// "Logout everywhere" — drops every session for the user.
pub async fn destroy_all_for_user(pool: &PgPool, user: UserId) -> Result<u64> {
    let res = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user.0)
        .execute(pool)
        .await
        .context("session::destroy_all_for_user")?;
    Ok(res.rows_affected())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionListing {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
}

pub async fn list_for_user(pool: &PgPool, user: UserId) -> Result<Vec<SessionListing>> {
    let rows: Vec<SessionListing> = sqlx::query_as(
        "SELECT id, created_at, last_used_at, expires_at, ip_hash, user_agent_hash \
         FROM sessions WHERE user_id = $1 ORDER BY last_used_at DESC",
    )
    .bind(user.0)
    .fetch_all(pool)
    .await
    .context("session::list_for_user")?;
    Ok(rows)
}

/// Targeted revoke — succeeds only when `(id, user_id)` match, so a user can
/// never destroy another user's session.
pub async fn destroy_for_user(pool: &PgPool, user: UserId, session_id: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM sessions WHERE id = $1 AND user_id = $2")
        .bind(session_id)
        .bind(user.0)
        .execute(pool)
        .await
        .context("session::destroy_for_user")?;
    Ok(res.rows_affected() > 0)
}

/// Builds the `Set-Cookie` header value for a session id.
pub fn build_cookie(cfg: &SessionConfig, value: String) -> Cookie<'static> {
    let mut c = Cookie::new(cfg.cookie_name.clone(), value);
    c.set_http_only(true);
    c.set_secure(cfg.cookie_secure);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    if !cfg.cookie_domain.is_empty() {
        c.set_domain(cfg.cookie_domain.clone());
    }
    c.set_max_age(tower_cookies::cookie::time::Duration::days(i64::from(
        cfg.absolute_timeout_days,
    )));
    c
}

/// Cookie that clears `_sm_session` on the client. Used by /auth/logout and
/// whenever lookup returns Expired.
pub fn clear_cookie(cfg: &SessionConfig) -> Cookie<'static> {
    let mut c = Cookie::new(cfg.cookie_name.clone(), "");
    c.set_http_only(true);
    c.set_secure(cfg.cookie_secure);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    if !cfg.cookie_domain.is_empty() {
        c.set_domain(cfg.cookie_domain.clone());
    }
    c.set_max_age(tower_cookies::cookie::time::Duration::seconds(0));
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_base64url_nopad_43_chars() {
        let id = generate_session_id();
        assert_eq!(id.len(), 43);
        assert!(!id.contains('=') && !id.contains('+') && !id.contains('/'));
    }

    #[test]
    fn build_cookie_marks_httponly_samesite_lax() {
        let cfg = SessionConfig::default();
        let c = build_cookie(&cfg, "tok".into());
        assert_eq!(c.name(), "_sm_session");
        assert_eq!(c.value(), "tok");
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Lax));
    }

    #[test]
    fn clear_cookie_zeroes_max_age() {
        let cfg = SessionConfig::default();
        let c = clear_cookie(&cfg);
        assert_eq!(c.value(), "");
        assert_eq!(
            c.max_age(),
            Some(tower_cookies::cookie::time::Duration::seconds(0))
        );
    }
}
