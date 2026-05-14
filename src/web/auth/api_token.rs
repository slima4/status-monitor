//! API-token Bearer middleware + email-verified gate extractor.
//!
//! The middleware runs ahead of the route handler. If the request carries
//! `Authorization: Bearer sm_live_…`, it looks the token up (argon2-verify
//! against rows matching `token_prefix`), inserts an
//! [`AuthContext::ApiToken`] into request extensions, and lazily updates
//! `api_tokens.last_used_at`. Anything else falls through — the session
//! extractor handles cookies later.
//!
//! Invalid tokens (no row, all rows failed verify) short-circuit with 401
//! `INVALID_TOKEN`. A missing or non-Bearer `Authorization` header is **not**
//! an error — the cookie path may still succeed.
//!
//! [`VerifiedCurrentUser`] is the type-level constraint required by
//! verification-sensitive endpoints (AUTH §5.4). Adding a new endpoint to that
//! list means updating the table in the spec AND swapping the extractor.

use axum::extract::{FromRef, FromRequestParts, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};
use chrono::{DateTime, Utc};

use crate::api::error::{ApiError, ApiErrorBody, codes};
use crate::app::AppState;
use crate::auth::api_tokens;
use crate::error::{AppError, Result};

use super::{AuthContext, CurrentUser};

/// Middleware entry point. Mount on the API router so it runs ahead of any
/// `FromRequestParts` impl that reads `AuthContext`.
pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let raw = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    let Some(raw) = raw else {
        return next.run(req).await;
    };
    if !raw.starts_with(api_tokens::TOKEN_PREFIX) {
        // Some other Bearer scheme — leave it for the session path / future
        // flows to interpret. Don't 401 here.
        return next.run(req).await;
    }

    let Some(pool) = state.db.as_ref() else {
        return unauthorized();
    };
    let prefix_len = state.cfg.auth.api_tokens.prefix_visible_chars as usize;
    match api_tokens::lookup_by_raw(pool, raw, prefix_len).await {
        Ok(api_tokens::LookupOutcome::Active(row)) => {
            let token_id = row.id;
            req.extensions_mut().insert(AuthContext::ApiToken {
                user_id: row.user_id,
                token_id,
            });
            // last_used_at debounce: most requests hit the cache and skip
            // the UPDATE entirely. Only spawn when the cache says a write is
            // due — saves a task allocation per Bearer request on the >99%
            // no-op path.
            if api_tokens::should_touch(&state.api_token_debounce, token_id) {
                let cache = state.api_token_debounce.clone();
                let pool = pool.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        api_tokens::touch_last_used_debounced(&pool, &cache, token_id).await
                    {
                        tracing::warn!(
                            error = %err,
                            "api_tokens::touch_last_used_debounced failed",
                        );
                    }
                });
            }
            next.run(req).await
        }
        Ok(api_tokens::LookupOutcome::Invalid) => unauthorized(),
        Err(err) => {
            tracing::warn!(error = %err, "api token lookup failed");
            unauthorized()
        }
    }
}

fn unauthorized() -> Response {
    let body = ApiError {
        error: ApiErrorBody::new(codes::INVALID_TOKEN, "API token is invalid or expired"),
    };
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

/// Extractor that enforces `email_verified_at IS NOT NULL`. Required by:
/// - `POST /api/v1/orgs/{id}/invitations` (inviter shown to recipients)
/// - `POST /api/v1/orgs/{id}/incidents/{id}/public-narrate` (text reaches public)
/// - `POST /api/v1/me/api-tokens` (compromised unverified account exfiltrates)
///
/// The type-system carries the constraint so handler signatures advertise the
/// requirement.
#[derive(Debug, Clone, Copy)]
pub struct VerifiedCurrentUser(pub CurrentUser);

impl<S> FromRequestParts<S> for VerifiedCurrentUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let app_state = AppState::from_ref(state);
        let user = CurrentUser::from_request_parts(parts, state).await?;
        let pool = app_state.db.as_ref().ok_or(AppError::Unauthorized)?;
        let row: Option<(Option<DateTime<Utc>>,)> =
            sqlx::query_as("SELECT email_verified_at FROM users WHERE id = $1")
                .bind(user.0.0)
                .fetch_optional(pool)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("verify lookup: {e}")))?;
        let verified = row.and_then(|(v,)| v).is_some();
        if !verified {
            return Err(AppError::forbidden_code(
                codes::EMAIL_NOT_VERIFIED,
                "this action requires a verified email",
            ));
        }
        Ok(Self(user))
    }
}
