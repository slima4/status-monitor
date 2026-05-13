use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use url::Host;
use uuid::Uuid;

use crate::api::redaction::{REDACTED, Redacted};
use crate::app::AppState;
use crate::domain::{AlertChannel, NewTarget, Target, TargetAlerts, TargetUpdate};
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
) -> Result<Redacted<Vec<Target>>> {
    Ok(Redacted::new(state.target_store.list(query.into()).await?))
}

pub async fn get(State(state): State<AppState>, Path(id): Path<Uuid>) -> Result<Redacted<Target>> {
    match state.target_store.get(id).await? {
        Some(t) => Ok(Redacted::new(t)),
        None => Err(AppError::NotFound("target not found".into())),
    }
}

pub async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewTarget>,
) -> Result<(StatusCode, Redacted<Target>)> {
    let guard = ssrf_guard(&state);
    validate_new_target(&new, &guard)?;
    let t = state.target_store.create(new).await?;
    Ok((StatusCode::CREATED, Redacted::new(t)))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(update): Json<TargetUpdate>,
) -> Result<Redacted<Target>> {
    if let Some(check) = &update.check {
        validate_check(check, &ssrf_guard(&state))?;
    }
    if let Some(alerts) = &update.alerts {
        validate_alerts(alerts)?;
    }
    match state.target_store.update(id, update).await? {
        Some(t) => Ok(Redacted::new(t)),
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
) -> Result<(StatusCode, Redacted<Vec<Target>>)> {
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
    Ok((StatusCode::CREATED, Redacted::new(out)))
}

fn ssrf_guard(state: &AppState) -> SsrfGuard {
    SsrfGuard::new(state.cfg.security.allow_private_targets)
}

fn validate_new_target(new: &NewTarget, guard: &SsrfGuard) -> Result<()> {
    validate_check(&new.check, guard)?;
    validate_alerts(&new.alerts)
}

fn validate_alerts(alerts: &TargetAlerts) -> Result<()> {
    for (channel, cfg) in alerts.iter() {
        if cfg.after_failures == 0 {
            return Err(AppError::BadRequest(format!(
                "alerts.{}: after_failures must be >= 1",
                channel.as_str()
            )));
        }
        match channel {
            AlertChannel::Email => {
                if cfg.to.is_empty() {
                    return Err(AppError::BadRequest(
                        "alerts.email: 'to' must contain at least one recipient".into(),
                    ));
                }
                for addr in &cfg.to {
                    if !addr.contains('@') {
                        return Err(AppError::BadRequest(format!(
                            "alerts.email: '{addr}' is not a valid email address"
                        )));
                    }
                }
            }
            _ => {
                if !cfg.to.is_empty() {
                    return Err(AppError::BadRequest(format!(
                        "alerts.{}: 'to' is only valid for the email channel",
                        channel.as_str()
                    )));
                }
            }
        }
    }
    Ok(())
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
            // Plain http already sends creds in the clear, so this rule only
            // protects the https + forged-cert MITM path that bypasses the
            // confidentiality the operator was relying on.
            if !http.verify_tls
                && scheme == "https"
                && (http.basic_auth.is_some() || http.bearer_token.is_some())
            {
                return Err(AppError::BadRequest(
                    "verify_tls = false cannot be combined with basic_auth or bearer_token over https — credentials would be exposed to any host presenting a forged certificate".into(),
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
        CheckSpec::TlsCert(cert) => {
            if cert.host.is_empty() {
                return Err(AppError::BadRequest("tls_cert host required".into()));
            }
            if cert.port == 0 {
                return Err(AppError::BadRequest("tls_cert port must be > 0".into()));
            }
            if cert.warn_days <= cert.critical_days {
                return Err(AppError::BadRequest(
                    "tls_cert warn_days must be > critical_days".into(),
                ));
            }
            let host = cert
                .host
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(&cert.host);
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
