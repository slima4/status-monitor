//! `/api/v1/me/api-tokens` — list, create, rename, revoke.
//!
//! The raw token is returned exactly once (on create); subsequent reads only
//! ever expose the visible prefix. Creation requires a verified email
//! (`VerifiedCurrentUser`) — a compromised unverified account
//! could otherwise exfiltrate via a fresh API token without ever proving
//! mailbox control.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::api_tokens as tokens;
use crate::error::{AppError, Result};
use crate::web::auth::api_token::VerifiedCurrentUser;
use crate::web::{CurrentOrg, CurrentUser};

/// Max length of a token name. Anything longer is almost certainly an
/// accident (or an attempt to fill the table with junk).
const MAX_NAME_LEN: usize = 80;

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewApiTokenRequest {
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NewApiTokenResponse {
    pub id: Uuid,
    pub name: String,
    /// Raw token shown ONCE — clients must store it now or regenerate.
    pub token: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiTokenView {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenameApiTokenRequest {
    pub name: String,
}

pub async fn create(
    State(state): State<AppState>,
    VerifiedCurrentUser(CurrentUser(user_id)): VerifiedCurrentUser,
    CurrentOrg(org): CurrentOrg,
    Json(req): Json<NewApiTokenRequest>,
) -> Result<(StatusCode, Json<NewApiTokenResponse>)> {
    let name = validate_name(&req.name)?;
    let pool = state.require_db()?;
    // Tokens are user-scoped; the cap is read from the active org's plan so a
    // user acting in two orgs sees each org's plan limit, not a single global.
    let max_tokens = i64::from(
        state
            .quotas
            .limit_for_org(org)
            .await?
            .max_api_tokens_per_user,
    );
    let prefix_len = state.cfg.auth.api_tokens.prefix_visible_chars as usize;
    // Atomic count-in-INSERT; no racy handler pre-check.
    let created = tokens::create(pool, user_id, name, prefix_len, max_tokens).await?;
    Ok((
        StatusCode::CREATED,
        Json(NewApiTokenResponse {
            id: created.id,
            name: created.name,
            token: created.token,
            prefix: created.prefix,
            created_at: created.created_at,
        }),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<Vec<ApiTokenView>>> {
    let pool = state.require_db()?;
    let rows = tokens::list_for_user(pool, user_id).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ApiTokenView {
                id: r.id,
                name: r.name,
                prefix: r.token_prefix,
                created_at: r.created_at,
                last_used_at: r.last_used_at,
                expires_at: r.expires_at,
            })
            .collect(),
    ))
}

pub async fn rename(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameApiTokenRequest>,
) -> Result<StatusCode> {
    let name = validate_name(&req.name)?;
    let pool = state.require_db()?;
    let updated = tokens::rename_for_user(pool, user_id, id, name).await?;
    if !updated {
        return Err(AppError::not_found(
            codes::TOKEN_NOT_FOUND,
            "token not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let pool = state.require_db()?;
    let removed = tokens::delete_for_user(pool, user_id, id).await?;
    if !removed {
        return Err(AppError::not_found(
            codes::TOKEN_NOT_FOUND,
            "token not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn validate_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request_field(
            codes::TOKEN_NAME_INVALID,
            "name must not be empty",
            "name",
        ));
    }
    if trimmed.len() > MAX_NAME_LEN {
        return Err(AppError::bad_request_field(
            codes::TOKEN_NAME_INVALID,
            format!("name must be at most {MAX_NAME_LEN} characters"),
            "name",
        ));
    }
    Ok(trimmed)
}
