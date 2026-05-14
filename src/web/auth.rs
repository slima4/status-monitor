//! Request → org resolution + cookie-backed session lookup.
//!
//! [`CurrentOrg`] is the only extractor that hands a handler an `OrgId`.
//! Combined with the org-scoped repositories in `src/storage/`, this is what
//! makes "forgetting to scope a query" a compile error rather than a security
//! incident: the repos require an `OrgId`, and the only place to obtain one
//! inside a request is this extractor.
//!
//! Resolution order for [`Session`]:
//!
//! 1. If an explicit `Session` is in request extensions (test fixtures
//!    stamp one via `from_fn`), use it as-is.
//! 2. Else, if a session cookie is present, look it up in `sessions`.
//!    A live row applies idle + absolute timeouts and lazily refreshes
//!    `last_used_at` (debounced per [`auth::session`]).
//! 3. Else, return an empty session (anonymous).
//!
//! Membership and personal-org SQL lives in [`crate::storage::orgs`].

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use std::convert::Infallible;
use tower_cookies::Cookies;

use crate::app::AppState;
use crate::auth::session as session_store;
use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};
use crate::storage::orgs::{is_active_member, personal_org_for_user};

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

/// Caller identity extracted from the session. Returns 401 when no user is
/// attached. Handlers that need both the active org and the caller use
/// [`CurrentOrg`] and [`CurrentUser`] together.
#[derive(Debug, Clone, Copy)]
pub struct CurrentUser(pub UserId);

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
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
