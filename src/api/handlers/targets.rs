use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use url::Host;
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::{NewTarget, Target, TargetUpdate};
use crate::error::{AppError, Result};
use crate::security::SsrfGuard;
use crate::storage::TargetFilter;

const BULK_MAX: usize = 10_000;
const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
    pub tag: Option<String>,
    pub enabled: Option<bool>,
}

impl From<ListQuery> for TargetFilter {
    fn from(q: ListQuery) -> Self {
        Self {
            limit: q.limit,
            offset: q.offset,
            tag: q.tag,
            enabled: q.enabled,
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Target>>> {
    let items = state.target_store.list(query.into()).await?;
    Ok(Json(items))
}

pub async fn get(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Target>> {
    match state.target_store.get(id).await? {
        Some(t) => Ok(Json(t)),
        None => Err(AppError::NotFound("target not found".into())),
    }
}

pub async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewTarget>,
) -> Result<(StatusCode, Json<Target>)> {
    let guard = ssrf_guard(&state);
    validate_new_target(&new, &guard)?;
    let t = state.target_store.create(new).await?;
    Ok((StatusCode::CREATED, Json(t)))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(update): Json<TargetUpdate>,
) -> Result<Json<Target>> {
    if let Some(check) = &update.check {
        validate_check(check, &ssrf_guard(&state))?;
    }
    match state.target_store.update(id, update).await? {
        Some(t) => Ok(Json(t)),
        None => Err(AppError::NotFound("target not found".into())),
    }
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<StatusCode> {
    if state.target_store.delete(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("target not found".into()))
    }
}

pub async fn bulk_create(
    State(state): State<AppState>,
    Json(items): Json<Vec<NewTarget>>,
) -> Result<(StatusCode, Json<Vec<Target>>)> {
    if items.is_empty() {
        return Err(AppError::BadRequest("empty bulk payload".into()));
    }
    if items.len() > BULK_MAX {
        return Err(AppError::PayloadTooLarge(format!(
            "bulk size {} exceeds max {BULK_MAX}",
            items.len()
        )));
    }
    let guard = ssrf_guard(&state);
    for new in &items {
        validate_new_target(new, &guard)?;
    }
    let out = state.target_store.bulk_create(items).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

fn ssrf_guard(state: &AppState) -> SsrfGuard {
    SsrfGuard::new(state.cfg.security.allow_private_targets)
}

fn validate_new_target(new: &NewTarget, guard: &SsrfGuard) -> Result<()> {
    validate_check(&new.check, guard)
}

fn validate_check(check: &crate::domain::CheckSpec, guard: &SsrfGuard) -> Result<()> {
    use crate::domain::CheckSpec;
    match check {
        CheckSpec::Http(http) => {
            let scheme = http.url.scheme();
            if !ALLOWED_SCHEMES.contains(&scheme) {
                return Err(AppError::BadRequest(format!(
                    "url scheme '{scheme}' not allowed"
                )));
            }
            match http.url.host() {
                Some(Host::Ipv4(v4)) => check_ip(IpAddr::V4(v4), guard)?,
                Some(Host::Ipv6(v6)) => check_ip(IpAddr::V6(v6), guard)?,
                Some(Host::Domain("")) => {
                    return Err(AppError::BadRequest("url missing host".into()));
                }
                Some(Host::Domain(_)) => {}
                None => return Err(AppError::BadRequest("url missing host".into())),
            }
        }
        CheckSpec::Tcp(tcp) => {
            if tcp.host.is_empty() {
                return Err(AppError::BadRequest("tcp host required".into()));
            }
            if tcp.port == 0 {
                return Err(AppError::BadRequest("tcp port must be > 0".into()));
            }
            let host = tcp
                .host
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(&tcp.host);
            if let Ok(ip) = host.parse::<IpAddr>() {
                check_ip(ip, guard)?;
            }
        }
    }
    Ok(())
}

fn check_ip(ip: IpAddr, guard: &SsrfGuard) -> Result<()> {
    guard
        .check(ip)
        .map_err(|err| AppError::BadRequest(err.to_string()))
}
