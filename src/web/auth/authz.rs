//! Scope-gated extractors.
//!
//! [`Authorized<R>`] resolves the caller's org via the same membership-checked
//! path as [`CurrentOrg`] (header-driven) and additionally asserts that an API
//! token carries the scope `R` requires. [`ScopedOrgPath<R>`] is its companion
//! for owner-keyed `:id` path routes (status-page settings) where the org comes
//! from the URL, not the header: it asserts the scope and, for an org-bound
//! token, that the path org matches the binding — ownership stays with the
//! handler's `require_owner`. Session auth is unscoped and always passes the
//! scope check; only [`AuthContext::ApiToken`] is gated.

use std::marker::PhantomData;

use axum::extract::{FromRef, FromRequestParts, Path};
use axum::http::request::Parts;
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::scope::{Scope, ScopeSet};
use crate::domain::{OrgId, WriteSource};
use crate::error::{AppError, Result};

use super::{AuthContext, CurrentOrg};

/// Compile-time binding of a handler to the scope it requires. A new scope =
/// a new marker type; the extractors below are untouched (open/closed).
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
scope_marker!(TargetsDelete => Scope::TargetsDelete);
scope_marker!(TargetsExecute => Scope::TargetsExecute);
scope_marker!(ChannelsRead => Scope::ChannelsRead);
scope_marker!(ChannelsWrite => Scope::ChannelsWrite);
scope_marker!(ChannelsDelete => Scope::ChannelsDelete);
scope_marker!(ChannelsExecute => Scope::ChannelsExecute);
scope_marker!(IncidentsWrite => Scope::IncidentsWrite);
scope_marker!(MaintenanceRead => Scope::MaintenanceRead);
scope_marker!(MaintenanceWrite => Scope::MaintenanceWrite);
scope_marker!(MaintenanceDelete => Scope::MaintenanceDelete);
scope_marker!(StatusPageRead => Scope::StatusPageRead);
scope_marker!(StatusPageWrite => Scope::StatusPageWrite);
scope_marker!(StatusPageDelete => Scope::StatusPageDelete);

/// The single place the `INSUFFICIENT_SCOPE` 403 is minted.
fn insufficient_scope(required: Scope) -> AppError {
    AppError::forbidden_code(
        codes::INSUFFICIENT_SCOPE,
        format!(
            "API token is missing the required scope `{}`",
            required.as_str()
        ),
    )
}

/// Asserts that an API token in the request carries `required` (sessions pass).
fn assert_scope(parts: &Parts, required: Scope) -> Result<()> {
    if let Some(AuthContext::ApiToken { scopes, .. }) = parts.extensions.get::<AuthContext>()
        && !scopes.allows(required)
    {
        return Err(insufficient_scope(required));
    }
    Ok(())
}

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
        // Resolve the org first (auth, membership, and the org binding) so a
        // missing/invalid X-Uptimepage-Org yields the same 400 for every token
        // — then gate on scope. Scope is checked only for API tokens; sessions
        // are unscoped and always pass.
        let CurrentOrg(org) = CurrentOrg::from_request_parts(parts, state).await?;
        assert_scope(parts, R::SCOPE)?;
        Ok(Authorized(org, PhantomData))
    }
}

/// Scope + org-binding gate for owner-keyed `:id` path routes (status-page
/// settings). The org comes from the URL, so this asserts the token scope and,
/// for an org-bound token, that the path org equals the binding (else
/// `ORG_HEADER_MISMATCH`). Ownership is enforced separately by the handler's
/// `require_owner` (which is stricter than membership, so no `is_active_member`
/// is duplicated here). Carries no payload — adding it to a handler *is* the
/// gate. Assumes a single `{id}` path param (true for every status-page route);
/// a route with extra path params would need an explicitly-named extractor.
pub struct ScopedOrgPath<R: RequiredScope>(pub PhantomData<R>);

impl<S, R> FromRequestParts<S> for ScopedOrgPath<R>
where
    S: Send + Sync,
    AppState: FromRef<S>,
    R: RequiredScope,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        assert_scope(parts, R::SCOPE)?;
        if let Some(AuthContext::ApiToken {
            org: Some(bound), ..
        }) = parts.extensions.get::<AuthContext>()
        {
            let bound = *bound;
            let Path(id) = Path::<Uuid>::from_request_parts(parts, state)
                .await
                .map_err(|_| AppError::not_found(codes::ORG_NOT_FOUND, "organisation not found"))?;
            if OrgId(id) != bound {
                return Err(AppError::forbidden_code(
                    codes::ORG_HEADER_MISMATCH,
                    "this token is bound to a different org",
                ));
            }
        }
        Ok(ScopedOrgPath(PhantomData))
    }
}

/// The request's API-token scopes, or `None` for a session. For handlers whose
/// required scope depends on the request body (e.g. bulk delete vs bulk edit);
/// pair with [`CurrentOrg`] for the org gate.
pub struct TokenScopes(pub Option<ScopeSet>);

impl TokenScopes {
    pub fn require(&self, scope: Scope) -> Result<()> {
        match &self.0 {
            Some(scopes) if !scopes.allows(scope) => Err(insufficient_scope(scope)),
            _ => Ok(()),
        }
    }
}

impl<S> FromRequestParts<S> for TokenScopes
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self> {
        Ok(TokenScopes(match parts.extensions.get::<AuthContext>() {
            Some(AuthContext::ApiToken { scopes, .. }) => Some(scopes.clone()),
            _ => None,
        }))
    }
}

/// The provider sets this token in its User-Agent so a Terraform-driven write
/// is badged distinctly from a hand-rolled API call.
const TERRAFORM_UA_MARKER: &str = "terraform-provider-uptimepage";

/// Where the current request writes from, for the managed-by badge. A cookie
/// session is [`WriteSource::Ui`]; a Bearer token is [`WriteSource::Terraform`]
/// when its User-Agent carries the provider marker, else [`WriteSource::Api`].
/// Only [`AuthContext::ApiToken`] is ever in extensions, so anything else is a
/// browser session.
pub struct RequestSource(pub WriteSource);

impl<S> FromRequestParts<S> for RequestSource
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        let source = match parts.extensions.get::<AuthContext>() {
            Some(AuthContext::ApiToken { .. }) => {
                let is_terraform = parts
                    .headers
                    .get(axum::http::header::USER_AGENT)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|ua| ua.contains(TERRAFORM_UA_MARKER));
                if is_terraform {
                    WriteSource::Terraform
                } else {
                    WriteSource::Api
                }
            }
            _ => WriteSource::Ui,
        };
        Ok(RequestSource(source))
    }
}
