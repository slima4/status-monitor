use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use url::Host;
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::{CheckSpec, NewTarget, Target, TargetUpdate};
use crate::error::{AppError, Result};
use crate::security::SsrfGuard;
use crate::storage::TargetFilter;

const BULK_MAX: usize = 10_000;
const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

/// Wire-level placeholder substituted for populated credentials in API responses.
/// Re-submitting it on `PATCH` is rejected so a `GET → PATCH` round-trip cannot
/// silently overwrite the real value with the sentinel.
const REDACTED: &str = "***";

fn redact_secrets(check: &mut CheckSpec) {
    if let CheckSpec::Http(http) = check {
        if let Some((u, p)) = http.basic_auth.as_mut() {
            *u = REDACTED.to_string();
            *p = REDACTED.to_string();
        }
        if let Some(token) = http.bearer_token.as_mut() {
            *token = REDACTED.to_string();
        }
    }
}

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
    let mut items = state.target_store.list(query.into()).await?;
    for t in &mut items {
        redact_secrets(&mut t.check);
    }
    Ok(Json(items))
}

pub async fn get(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Json<Target>> {
    match state.target_store.get(id).await? {
        Some(mut t) => {
            redact_secrets(&mut t.check);
            Ok(Json(t))
        }
        None => Err(AppError::NotFound("target not found".into())),
    }
}

pub async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewTarget>,
) -> Result<(StatusCode, Json<Target>)> {
    let guard = ssrf_guard(&state);
    validate_new_target(&new, &guard)?;
    let mut t = state.target_store.create(new).await?;
    redact_secrets(&mut t.check);
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
        Some(mut t) => {
            redact_secrets(&mut t.check);
            Ok(Json(t))
        }
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
    let mut out = state.target_store.bulk_create(items).await?;
    for t in &mut out {
        redact_secrets(&mut t.check);
    }
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
            if let Some((u, p)) = &http.basic_auth
                && (u == REDACTED || p == REDACTED)
            {
                return Err(AppError::BadRequest(
                    "basic_auth contains redaction sentinel — re-supply the real credential".into(),
                ));
            }
            if http.bearer_token.as_deref() == Some(REDACTED) {
                return Err(AppError::BadRequest(
                    "bearer_token contains redaction sentinel — re-supply the real credential"
                        .into(),
                ));
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
