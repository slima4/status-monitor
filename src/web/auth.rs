//! Request → org resolution.
//!
//! [`CurrentOrg`] is the only extractor that hands a handler an `OrgId`.
//! Combined with the org-scoped repositories in `src/storage/`, this is what
//! makes "forgetting to scope a query" a compile error rather than a security
//! incident: the repos require an `OrgId`, and the only place to obtain one
//! inside a request is this extractor.
//!
//! Two resolution modes:
//!
//! * `tenancy.enabled = false` (self-host) — every request resolves to
//!   `AppState::default_org_id`. No session inspection, no DB read.
//! * `tenancy.enabled = true`  (SaaS) — reads `Session::active_org_id`. If the
//!   user is not (or no longer) an active member of that org → 403. If no
//!   active org is selected, falls back to the user's personal org. The
//!   `Session` extractor itself is still a stub — wiring real sessions
//!   belongs to the auth spec, not this one.
//!
//! Membership and personal-org SQL lives in [`crate::storage::orgs`], the
//! single owner of access to the `organizations` and `memberships` tables.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use std::convert::Infallible;

use crate::app::AppState;
use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};
use crate::storage::orgs::{is_active_member, personal_org_for_user};

#[derive(Debug, Clone)]
pub struct User {
    pub id: UserId,
    pub email: String,
}

/// Authenticated session. The auth backend is not yet wired; for now this
/// extractor always yields an empty session, so [`CurrentOrg`] falls through
/// to the default-org path when tenancy is disabled. Once `AUTH-spec.md`
/// lands, this becomes a real cookie / token lookup.
#[derive(Debug, Default, Clone)]
pub struct Session {
    pub user: Option<User>,
    /// Org actively selected by the user via the org picker. `None` means
    /// "fall back to my personal org".
    pub active_org_id: Option<OrgId>,
}

impl Session {
    pub fn user_id(&self) -> Option<UserId> {
        self.user.as_ref().map(|u| u.id)
    }
}

impl<S> FromRequestParts<S> for Session
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(_parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::default())
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
