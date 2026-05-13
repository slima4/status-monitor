//! Session-cookie scaffolding.
//!
//! v1 has no authentication. This extractor exists so that adding a real
//! session lookup later doesn't require touching every web handler:
//! flip `Option<User>` to required `User` and the compiler enumerates
//! the call-sites that need a login redirect.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;

#[derive(Debug, Clone)]
pub struct User {
    pub id: uuid::Uuid,
    pub email: String,
}

#[derive(Debug, Default, Clone)]
pub struct Session {
    pub user: Option<User>,
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
