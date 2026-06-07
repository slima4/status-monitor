//! Operator (instance-admin) HTTP surface: regions + agents. Every handler is
//! gated by [`OperatorAuth`] (a static bearer secret), cross-tenant, and kept
//! off the tenant `/api/v1` surface. Agent creation mints the agent's token and
//! returns it exactly once.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::codes;
use crate::app::AppState;
use crate::error::{AppError, Result};
use crate::storage::operator::{DeleteRegion, OperatorRepo};
use crate::web::OperatorAuth;

const MAX_NAME: usize = 80;
const MAX_REGION_ID: usize = 40;
const MAX_LOCATION: usize = 120;

fn repo(state: &AppState) -> Result<OperatorRepo> {
    Ok(OperatorRepo::new(state.require_db()?.clone()))
}

fn validate_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_NAME {
        return Err(AppError::bad_request(
            codes::INVALID_NAME,
            format!("name must be 1..={MAX_NAME} characters"),
        ));
    }
    Ok(trimmed)
}

/// Region ids are slugs: lowercase alphanumerics and hyphens, no leading or
/// trailing hyphen. They surface in URLs and ClickHouse `region` values, so
/// keep them tidy and stable.
fn validate_region_id(id: &str) -> Result<&str> {
    let id = id.trim();
    let ok = !id.is_empty()
        && id.len() <= MAX_REGION_ID
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-');
    if !ok {
        return Err(AppError::bad_request(
            codes::REGION_INVALID,
            "region id must be a lowercase slug (a-z, 0-9, -)",
        ));
    }
    Ok(id)
}

fn validate_location(location: Option<&str>) -> Result<Option<&str>> {
    if let Some(l) = location
        && l.len() > MAX_LOCATION
    {
        return Err(AppError::bad_request(
            codes::REGION_INVALID,
            format!("location must be at most {MAX_LOCATION} characters"),
        ));
    }
    Ok(location)
}

// ── regions ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RegionView {
    pub id: String,
    pub name: String,
    pub location: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct NewRegion {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub location: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateRegion {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub async fn list_regions(
    _: OperatorAuth,
    State(state): State<AppState>,
) -> Result<Json<Vec<RegionView>>> {
    let regions = repo(&state)?.list_regions().await?;
    Ok(Json(
        regions
            .into_iter()
            .map(|r| RegionView {
                id: r.id,
                name: r.name,
                location: r.location,
                enabled: r.enabled,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

pub async fn create_region(
    _: OperatorAuth,
    State(state): State<AppState>,
    Json(req): Json<NewRegion>,
) -> Result<StatusCode> {
    let id = validate_region_id(&req.id)?;
    let name = validate_name(&req.name)?;
    let location = validate_location(req.location.as_deref())?;
    if repo(&state)?.create_region(id, name, location).await? {
        Ok(StatusCode::CREATED)
    } else {
        Err(AppError::conflict(
            codes::REGION_EXISTS,
            "a region with that id already exists",
        ))
    }
}

pub async fn update_region(
    _: OperatorAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRegion>,
) -> Result<StatusCode> {
    if req.name.is_none() && req.location.is_none() && req.enabled.is_none() {
        return Err(AppError::bad_request(
            codes::EMPTY_PATCH,
            "set at least one of name, location, enabled",
        ));
    }
    let r = repo(&state)?;
    let mut found = false;
    if req.name.is_some() || req.location.is_some() {
        let name = req.name.as_deref().map(validate_name).transpose()?;
        let location = validate_location(req.location.as_deref())?;
        found |= r.update_region(&id, name, location).await?;
    }
    if let Some(enabled) = req.enabled {
        found |= r.set_region_enabled(&id, enabled).await?;
    }
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            codes::REGION_NOT_FOUND,
            "region not found",
        ))
    }
}

pub async fn delete_region(
    _: OperatorAuth,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    match repo(&state)?.delete_region(&id).await? {
        DeleteRegion::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteRegion::NotFound => Err(AppError::not_found(
            codes::REGION_NOT_FOUND,
            "region not found",
        )),
        DeleteRegion::InUse => Err(AppError::conflict(
            codes::REGION_IN_USE,
            "region still has agents or assigned targets",
        )),
    }
}

// ── agents ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AgentView {
    pub id: Uuid,
    pub region: String,
    pub name: String,
    pub enabled: bool,
    pub token_prefix: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct NewAgent {
    pub region: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct NewAgentResponse {
    pub id: Uuid,
    pub region: String,
    pub name: String,
    /// Raw token — shown exactly once; store it on the agent box now.
    pub token: String,
    pub token_prefix: String,
}

#[derive(Deserialize)]
pub struct UpdateAgent {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub async fn list_agents(
    _: OperatorAuth,
    State(state): State<AppState>,
) -> Result<Json<Vec<AgentView>>> {
    let agents = repo(&state)?.list_agents().await?;
    Ok(Json(
        agents
            .into_iter()
            .map(|a| AgentView {
                id: a.id,
                region: a.region,
                name: a.name,
                enabled: a.enabled,
                token_prefix: a.token_prefix,
                last_seen_at: a.last_seen_at,
                created_at: a.created_at,
            })
            .collect(),
    ))
}

pub async fn create_agent(
    _: OperatorAuth,
    State(state): State<AppState>,
    Json(req): Json<NewAgent>,
) -> Result<(StatusCode, Json<NewAgentResponse>)> {
    let region = validate_region_id(&req.region)?;
    let name = validate_name(&req.name)?;
    match repo(&state)?.create_agent(region, name).await? {
        Some((row, token)) => Ok((
            StatusCode::CREATED,
            Json(NewAgentResponse {
                id: row.id,
                region: row.region,
                name: row.name,
                token,
                token_prefix: row.token_prefix,
            }),
        )),
        None => Err(AppError::not_found(
            codes::REGION_NOT_FOUND,
            "region not found — create it first",
        )),
    }
}

pub async fn update_agent(
    _: OperatorAuth,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAgent>,
) -> Result<StatusCode> {
    if req.name.is_none() && req.enabled.is_none() {
        return Err(AppError::bad_request(
            codes::EMPTY_PATCH,
            "set at least one of name, enabled",
        ));
    }
    let r = repo(&state)?;
    let mut found = false;
    if let Some(name) = req.name.as_deref() {
        let name = validate_name(name)?;
        found |= r.rename_agent(id, name).await?;
    }
    if let Some(enabled) = req.enabled {
        found |= r.set_agent_enabled(id, enabled).await?;
    }
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            codes::AGENT_NOT_FOUND,
            "agent not found",
        ))
    }
}

pub async fn delete_agent(
    _: OperatorAuth,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    if repo(&state)?.delete_agent(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            codes::AGENT_NOT_FOUND,
            "agent not found",
        ))
    }
}
