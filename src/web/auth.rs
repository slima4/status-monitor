//! Request → org resolution + cookie- or token-backed authentication.
//!
//! Two auth paths feed the same extractor surface:
//!
//! 1. **Session cookie** — `_sm_session` resolved via [`crate::auth::session`].
//!    Carries `active_org_id` and falls back to the user's personal org.
//! 2. **API token** — `Authorization: Bearer sm_live_…` resolved by the
//!    [`api_token`] middleware ahead of routing. The token carries no active
//!    org, so handlers that need one must read an explicit org slug from the
//!    `X-Status-Monitor-Org` header — otherwise [`CurrentOrg`] returns 400
//!    `ORG_REQUIRED`.
//!
//! [`CurrentOrg`] is the only extractor that hands a handler an `OrgId`.
//! Combined with the org-scoped repositories in `src/storage/`, this is what
//! makes "forgetting to scope a query" a compile error rather than a security
//! incident: the repos require an `OrgId`, and the only place to obtain one
//! inside a request is this extractor.

pub mod api_token;
pub mod csrf;

/// `Authorization: Bearer <raw>` → trimmed `<raw>`, or `None` for anything
/// else. Used by both the API-token middleware (to decide whether to look the
/// token up) and the CSRF guard (to know a request is Bearer-authenticated and
/// therefore exempt from the cookie-targeted CSRF rule).
pub fn bearer_from_headers(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim)
}

use axum::extract::{FromRef, FromRequestParts};
use axum::http::HeaderName;
use axum::http::request::Parts;
use std::convert::Infallible;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::session as session_store;
use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};
use crate::storage::orgs::{is_active_member, personal_org_for_user};

/// Custom header used by API-token clients to scope writes/reads to a specific
/// org. Tokens carry no active-org state, so this header (or a future slug
/// path param) is the only way the handler learns which org to read/write.
pub const ORG_HEADER: HeaderName = HeaderName::from_static("x-status-monitor-org");

#[derive(Debug, Clone)]
pub struct User {
    pub id: UserId,
    pub email: String,
}

/// Authenticated session. The cookie-backed extractor populates this from the
/// `sessions` table; tests can short-circuit by stamping one into request
/// extensions ahead of the extractor.
#[derive(Debug, Default, Clone)]
pub struct Session {
    pub user: Option<User>,
    /// Active org selected by the user via the org picker, or set by signup.
    /// `None` means "fall back to my personal org".
    pub active_org_id: Option<OrgId>,
    /// Present iff this Session was constructed by the cookie path. Handlers
    /// that need to destroy or rotate the session (logout) reach for this.
    pub session_id: Option<String>,
}

impl Session {
    pub fn user_id(&self) -> Option<UserId> {
        self.user.as_ref().map(|u| u.id)
    }
}

/// Result of authentication, populated either by the API-token middleware
/// (Bearer path) or by the session extractor (cookie path). Lives in request
/// extensions; [`CurrentUser`] and [`CurrentOrg`] read it.
#[derive(Debug, Clone)]
pub enum AuthContext {
    /// Cookie-based browser session. Carries an `active_org_id` that the user
    /// chose via the org picker; falls back to their personal org.
    Session {
        user_id: UserId,
        session_id: String,
        active_org_id: Option<OrgId>,
    },
    /// `Authorization: Bearer sm_live_…`. No active org — handlers reach for
    /// `X-Status-Monitor-Org` (or a slug path param in future routes).
    ApiToken { user_id: UserId, token_id: Uuid },
}

impl AuthContext {
    pub fn user_id(&self) -> UserId {
        match self {
            AuthContext::Session { user_id, .. } | AuthContext::ApiToken { user_id, .. } => {
                *user_id
            }
        }
    }
}

impl<S> FromRequestParts<S> for Session
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(injected) = parts.extensions.get::<Session>().cloned() {
            return Ok(injected);
        }

        let app_state = AppState::from_ref(state);
        if !app_state.cfg.tenancy.enabled {
            return Ok(Session::default());
        }
        let Some(pool) = app_state.db.as_ref() else {
            return Ok(Session::default());
        };
        let Some(cookies) = parts.extensions.get::<Cookies>().cloned() else {
            return Ok(Session::default());
        };
        let cookie_name = app_state.cfg.auth.session.cookie_name.as_str();
        let Some(cookie_val) = cookies.get(cookie_name).map(|c| c.value().to_string()) else {
            return Ok(Session::default());
        };

        match session_store::lookup(pool, &app_state.cfg.auth.session, &cookie_val).await {
            Ok(session_store::LookupOutcome::Active(row)) => {
                if let Err(err) = session_store::touch_last_used_debounced(
                    pool,
                    &app_state.session_debounce,
                    &row.id,
                )
                .await
                {
                    tracing::warn!(error = %err, "touch_last_used_debounced failed");
                }
                let user = load_user(pool, row.user_id).await;
                Ok(Session {
                    user,
                    active_org_id: row.active_org_id,
                    session_id: Some(row.id),
                })
            }
            Ok(session_store::LookupOutcome::Expired) => {
                cookies.remove(
                    tower_cookies::Cookie::build((cookie_name.to_string(), String::new()))
                        .path("/")
                        .build(),
                );
                Ok(Session::default())
            }
            Ok(session_store::LookupOutcome::Missing) => Ok(Session::default()),
            Err(err) => {
                tracing::warn!(error = %err, "session lookup failed");
                Ok(Session::default())
            }
        }
    }
}

async fn load_user(pool: &sqlx::PgPool, user_id: UserId) -> Option<User> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT email::text FROM users WHERE id = $1 AND deleted_at IS NULL")
            .bind(user_id.0)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    row.map(|(email,)| User { id: user_id, email })
}

/// Caller identity extracted from either a session cookie or an API token.
/// Returns 401 if neither path produced a user. Handlers that need both the
/// active org and the caller use [`CurrentOrg`] and [`CurrentUser`] together.
#[derive(Debug, Clone, Copy)]
pub struct CurrentUser(pub UserId);

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        if let Some(ctx) = parts.extensions.get::<AuthContext>() {
            return Ok(CurrentUser(ctx.user_id()));
        }
        let session = Session::from_request_parts(parts, state)
            .await
            .expect("Session extractor is infallible");
        session
            .user_id()
            .map(CurrentUser)
            .ok_or(AppError::Unauthorized)
    }
}

/// Org id for the current request. Constructed by the extractor; never by
/// hand. Wrapping `OrgId` in a separate newtype keeps the "this came from the
/// request" provenance visible at the type level.
#[derive(Debug, Clone, Copy)]
pub struct CurrentOrg(pub OrgId);

impl<S> FromRequestParts<S> for CurrentOrg
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let app_state = AppState::from_ref(state);

        if !app_state.cfg.tenancy.enabled {
            return Ok(CurrentOrg(app_state.default_org_id));
        }

        // API-token path: tokens carry no active org. The caller MUST surface
        // the org explicitly (slug header today; future slug-path routes can
        // also feed this). Falling back to the personal org silently routes
        // API-token writes into the wrong org (the data-misdirection bug).
        if let Some(AuthContext::ApiToken { user_id, .. }) = parts.extensions.get::<AuthContext>() {
            let pool = app_state.db.as_ref().ok_or_else(|| {
                AppError::Other(anyhow::anyhow!(
                    "tenancy enabled but AppState.db is None — refusing to resolve org"
                ))
            })?;
            let user_id = *user_id;
            let org = explicit_org_from_header(parts, pool).await?;
            if !is_active_member(pool, user_id, org).await? {
                return Err(AppError::Forbidden);
            }
            return Ok(CurrentOrg(org));
        }

        let session = Session::from_request_parts(parts, state)
            .await
            .expect("Session extractor is infallible");

        let user_id = session.user_id().ok_or(AppError::Unauthorized)?;
        let pool = app_state.db.as_ref().ok_or_else(|| {
            AppError::Other(anyhow::anyhow!(
                "tenancy enabled but AppState.db is None — refusing to resolve org"
            ))
        })?;

        if let Some(active) = session.active_org_id {
            if !is_active_member(pool, user_id, active).await? {
                return Err(AppError::Forbidden);
            }
            return Ok(CurrentOrg(active));
        }

        let personal = personal_org_for_user(pool, user_id)
            .await?
            .ok_or(AppError::Forbidden)?;
        Ok(CurrentOrg(personal))
    }
}

/// Reads `X-Status-Monitor-Org` from the request headers and resolves it to
/// an org id by slug. Returns `ORG_REQUIRED` when absent and
/// `ORG_HEADER_INVALID` when the header value is malformed or names no org.
async fn explicit_org_from_header(parts: &Parts, pool: &sqlx::PgPool) -> Result<OrgId> {
    let raw = parts
        .headers
        .get(&ORG_HEADER)
        .ok_or_else(|| {
            AppError::bad_request(
                codes::ORG_REQUIRED,
                "API tokens must scope to an org via the X-Status-Monitor-Org header",
            )
        })?
        .to_str()
        .map_err(|_| {
            AppError::bad_request(
                codes::ORG_HEADER_INVALID,
                "X-Status-Monitor-Org is not UTF-8",
            )
        })?
        .trim();
    if raw.is_empty() {
        return Err(AppError::bad_request(
            codes::ORG_HEADER_INVALID,
            "X-Status-Monitor-Org is empty",
        ));
    }
    crate::storage::orgs::find_id_by_slug(pool, raw)
        .await?
        .ok_or_else(|| {
            AppError::bad_request(
                codes::ORG_HEADER_INVALID,
                "no organization matches X-Status-Monitor-Org",
            )
        })
}
