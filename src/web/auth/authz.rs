//! Scope-gated org extractor.
//!
//! `Authorized<R>` resolves the caller's org via the same membership-checked
//! path as [`CurrentOrg`] and additionally asserts that an API token carries
//! the scope `R` requires. Session auth is unscoped and always passes the
//! scope check; only [`AuthContext::ApiToken`] is gated. Swapping a handler's
//! `CurrentOrg(org): CurrentOrg` for `Authorized(org, _): Authorized<…>` is the
//! whole integration — the resolved `OrgId` threads through unchanged.

use std::marker::PhantomData;

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::scope::Scope;
use crate::domain::OrgId;
use crate::error::{AppError, Result};

use super::{AuthContext, CurrentOrg};

/// Compile-time binding of a handler to the scope it requires. A new scope =
/// a new marker type; the extractor below is untouched (open/closed).
pub trait RequiredScope {
    const SCOPE: Scope;
}

macro_rules! scope_marker {
    ($name:ident => $scope:expr) => {
        pub struct $name;
        impl RequiredScope for $name {
            const SCOPE: Scope = $scope;
        }
    };
}

scope_marker!(TargetsRead => Scope::TargetsRead);
scope_marker!(TargetsWrite => Scope::TargetsWrite);

/// Org id for the current request, gated on the scope `R`.
pub struct Authorized<R: RequiredScope>(pub OrgId, pub PhantomData<R>);

impl<S, R> FromRequestParts<S> for Authorized<R>
where
    S: Send + Sync,
    AppState: FromRef<S>,
    R: RequiredScope,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        // Resolve the org first (auth, membership, and any org binding) so a
        // missing/invalid X-Uptimepage-Org yields the same 400 for every token
        // — then gate on scope. Scope is checked only for API tokens; sessions
        // are unscoped and always pass.
        let CurrentOrg(org) = CurrentOrg::from_request_parts(parts, state).await?;
        if let Some(AuthContext::ApiToken { scopes, .. }) = parts.extensions.get::<AuthContext>()
            && !scopes.allows(R::SCOPE)
        {
            return Err(AppError::forbidden_code(
                codes::INSUFFICIENT_SCOPE,
                format!(
                    "API token is missing the required scope `{}`",
                    R::SCOPE.as_str()
                ),
            ));
        }
        Ok(Authorized(org, PhantomData))
    }
}
