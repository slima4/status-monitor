//! `/auth/*` endpoints: GitHub/Google OAuth login + callback, logout.
//!
//! Both providers share one start/finish runner; only Phase B (the upstream
//! identity fetch) dispatches per provider. The callback follows a strict
//! three-phase rule — no DB transaction held across upstream HTTP calls. New
//! users get a signup org auto-created in the same Phase C transaction that
//! links their identity; the resolved default org id is stamped onto the new
//! session row so the next request lands on a real org.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::header::USER_AGENT;
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::app::AppState;
use crate::auth::{
    OauthProvider, fingerprint, github, google,
    login_audit::{self, LoginAttempt, LoginMethod},
    oauth_login, oauth_state, session as session_store,
    url::safe_redirect_target,
};
use crate::config::OauthClientConfig;
use crate::error::{AppError, Result};
use crate::web::CurrentUser;

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub redirect_after: Option<String>,
    pub invitation: Option<String>,
}

/// Per-provider plumbing the shared runners dispatch on.
struct ProviderParts<'a> {
    cfg: &'a OauthClientConfig,
    method: LoginMethod,
    enabled: bool,
}

fn provider_parts(state: &AppState, provider: OauthProvider) -> ProviderParts<'_> {
    let auth = &state.cfg.auth;
    match provider {
        OauthProvider::Github => ProviderParts {
            cfg: &auth.github,
            method: LoginMethod::GithubOauth,
            enabled: auth.github_login_enabled(),
        },
        OauthProvider::Google => ProviderParts {
            cfg: &auth.google,
            method: LoginMethod::GoogleOauth,
            enabled: auth.google_login_enabled(),
        },
    }
}

/// 404, not 500 — scanner probes must not pollute the 5xx rate. Logs the
/// listed-but-misconfigured case so a half-set provider doesn't silently
/// look like deliberate policy.
fn unavailable(state: &AppState, provider: OauthProvider) -> AppError {
    let p = provider_parts(state, provider);
    if !p.cfg.is_configured() && state.cfg.auth.method_enabled(p.method.as_db_str()) {
        tracing::warn!(
            provider = provider.as_db_str(),
            "oauth login listed in enabled_methods but client_id/client_secret/redirect_url incomplete"
        );
    }
    AppError::not_found(
        "AUTH_METHOD_UNAVAILABLE",
        "this sign-in method is not enabled",
    )
}

pub async fn github_login(state: State<AppState>, q: Query<LoginQuery>) -> Result<Redirect> {
    start_login(state, q, OauthProvider::Github).await
}

pub async fn google_login(state: State<AppState>, q: Query<LoginQuery>) -> Result<Redirect> {
    start_login(state, q, OauthProvider::Google).await
}

async fn start_login(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
    provider: OauthProvider,
) -> Result<Redirect> {
    let pool = state.db.as_ref().ok_or_else(|| {
        AppError::Other(anyhow::anyhow!(
            "oauth login: no Postgres pool — auth requires tenancy mode"
        ))
    })?;
    let parts = provider_parts(&state, provider);
    if !parts.enabled {
        return Err(unavailable(&state, provider));
    }
    let cfg = parts.cfg;
    // Only same-origin paths survive: anything else gets dropped to None so
    // the callback redirects to `/`. Without this, `?redirect_after=https://evil.test`
    // turns a legit OAuth dance into an open-redirect into attacker territory.
    let redirect_after = q.redirect_after.as_deref().and_then(safe_redirect_target);

    // Resolve token → row id at this edge so the raw token is never at rest
    // in `oauth_states`; the id alone isn't replayable (accept still
    // requires the session-bound email to match).
    let invitation_id =
        crate::auth::invitations::resolve_pending_invitation_id(pool, q.invitation.as_deref())
            .await?;

    let s = oauth_state::generate_state();
    oauth_state::insert(
        pool,
        &s,
        provider.as_db_str(),
        redirect_after,
        invitation_id,
        None,
        None,
    )
    .await?;
    let url = match provider {
        OauthProvider::Github => github::authorize_url(cfg, &s),
        OauthProvider::Google => google::authorize_url(cfg, &s),
    };
    Ok(Redirect::to(&url))
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    /// Absent when the user denies consent (provider sends `error=` instead).
    pub code: Option<String>,
    pub state: String,
    pub error: Option<String>,
}

pub async fn github_callback(
    state: State<AppState>,
    q: Query<CallbackQuery>,
    ip: crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<axum::response::Response> {
    finish_login(state, q, ip, headers, cookies, OauthProvider::Github).await
}

pub async fn google_callback(
    state: State<AppState>,
    q: Query<CallbackQuery>,
    ip: crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<axum::response::Response> {
    finish_login(state, q, ip, headers, cookies, OauthProvider::Google).await
}

async fn finish_login(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
    crate::web::client_ip::ClientIp(client_ip): crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
    provider: OauthProvider,
) -> Result<axum::response::Response> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("oauth callback: no Postgres pool")))?;
    let parts = provider_parts(&state, provider);
    // Policy-gated like start_login — a state minted before the method was
    // switched off must not complete a sign-in for the rest of its TTL.
    if !parts.enabled {
        return Err(unavailable(&state, provider));
    }
    let (cfg, method) = (parts.cfg, parts.method);
    let salt = state.cfg.auth.fingerprint_salt.as_str();
    let ip_hash = fingerprint::hash_fingerprint(salt, &client_ip.to_string());
    let ua_value = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let ua_hash = fingerprint::hash_fingerprint(salt, ua_value);

    // Phase A: consume state. Single-use; expired, unknown, or minted for a
    // different dance (e.g. a connect-purpose provider) → 400.
    let consumed = match oauth_state::consume(pool, &q.state).await? {
        Some(c) if c.provider == provider.as_db_str() => c,
        _ => {
            login_audit::record_failure_anon(
                pool,
                method,
                ip_hash.as_deref(),
                ua_hash.as_deref(),
                "invalid_state",
            )
            .await;
            return Err(AppError::bad_request(
                "INVALID_STATE",
                "OAuth state is invalid or has expired",
            ));
        }
    };

    // Denied consent / provider error — state already burnt, back to /login.
    let Some(code) = q.code.as_deref().filter(|c| !c.is_empty()) else {
        let reason = if q.error.is_some() {
            "oauth_denied"
        } else {
            "missing_code"
        };
        login_audit::record_failure_anon(
            pool,
            method,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            reason,
        )
        .await;
        return Ok(Redirect::to("/login").into_response());
    };

    // Phase B: upstream HTTP. NO DB connection held here.
    let fetched = match provider {
        OauthProvider::Github => github::fetch_identity(&state.outbound_http, cfg, code).await,
        OauthProvider::Google => google::fetch_identity(&state.outbound_http, cfg, code).await,
    };
    let identity = match fetched {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(error = %err, provider = provider.as_db_str(), "oauth callback: phase B failed");
            login_audit::record_failure_anon(
                pool,
                method,
                ip_hash.as_deref(),
                ua_hash.as_deref(),
                "oauth_upstream_failed",
            )
            .await;
            return Err(AppError::Other(anyhow::anyhow!(
                "oauth callback: phase B: {err}"
            )));
        }
    };

    // Phase C: materialise user + identity, auto-create signup org for new
    // users, and resolve their default-org id for the session row. Fresh
    // transaction; no upstream calls.
    let resolved = oauth_login::upsert_identity_and_signup_org(pool, provider, &identity)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, provider = provider.as_db_str(), "oauth callback: phase C failed");
            e
        })?;
    if resolved.restored {
        tracing::info!(user_id = %resolved.user_id.0, "re-auth restored a soft-deleted account");
    }

    // The dance carried an invitation — redeem it now, server-side; the
    // session then opens directly in the joined org.
    let joined = match consumed.invitation_id {
        Some(id) => {
            crate::api::handlers::invitations::try_auto_accept(&state, resolved.user_id, id).await
        }
        None => None,
    };
    let active_org = joined.as_ref().map(|j| j.org_id).or(resolved.signup_org_id);

    // Session fixation: drop any pre-login session bound to this browser
    // before minting the new one. Without this an attacker who pre-seeded a
    // cookie inherits the just-authenticated session.
    let cookie_name = state.cfg.auth.session.cookie_name.as_str();
    if let Some(prev) = cookies.get(cookie_name).map(|c| c.value().to_string())
        && !prev.is_empty()
        && let Err(err) = session_store::destroy(pool, &prev).await
    {
        tracing::warn!(error = %err, "session fixation: pre-login destroy failed");
    }

    let created = session_store::create(
        pool,
        &state.cfg.auth.session,
        resolved.user_id,
        active_org,
        ip_hash.as_deref(),
        ua_hash.as_deref(),
    )
    .await?;

    // Audit post-commit: a failure here logs but the session is already valid.
    if let Err(err) = login_audit::record(
        pool,
        method,
        LoginAttempt {
            user_id: Some(resolved.user_id),
            success: true,
            ip_hash: ip_hash.as_deref(),
            user_agent_hash: ua_hash.as_deref(),
            failure_reason: None,
        },
    )
    .await
    {
        tracing::warn!(error = %err, "login_audit write failed (non-fatal)");
    }

    cookies.add(session_store::build_cookie(
        &state.cfg.auth.session,
        created.cookie_token,
    ));
    crate::web::login_hint::set(&cookies, &state.cfg.auth.session, method.as_db_str());
    if let Err(err) =
        crate::web::display_prefs::issue_cookies(&state, &cookies, resolved.user_id).await
    {
        tracing::warn!(error = %err, "display-preference cookie issue failed (non-fatal)");
    }

    // One-shot banners ride a flash cookie (unspoofable, fires once); only the
    // slug-validated `joined` stays a query param.
    let invite_missed = joined.is_none() && consumed.invitation_id.is_some();
    crate::web::flash::set(
        &cookies,
        crate::web::flash::Flash {
            restored: resolved.restored,
            invite_missed,
        },
        state.cfg.auth.session.cookie_secure,
        &state.cfg.auth.session.cookie_domain,
    );
    // Joined org outranks onboarding — the invitation is why they came; the
    // personal signup org keeps its default name, renameable in settings.
    let redirect = if let Some(j) = joined {
        format!("/?joined={}", crate::auth::url::url_encode(&j.org_slug))
    } else if invite_missed {
        "/".to_string()
    } else if resolved.is_new_user {
        "/onboarding/org".to_string()
    } else if resolved.restored {
        // Banner outranks redirect_after — the user must learn the account
        // came back.
        "/".to_string()
    } else {
        consumed
            .redirect_after
            .as_deref()
            .and_then(safe_redirect_target)
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    };
    Ok(Redirect::to(&redirect).into_response())
}

pub async fn logout(
    State(state): State<AppState>,
    cookies: Cookies,
) -> Result<axum::response::Response> {
    let cookie_name = state.cfg.auth.session.cookie_name.as_str();
    if let Some(c) = cookies.get(cookie_name)
        && let Some(pool) = state.db.as_ref()
    {
        let id = c.value().to_string();
        if let Err(err) = session_store::destroy(pool, &id).await {
            // Surface failures: silently dropping them leaves the DB row alive
            // while the browser thinks it's logged out — an attacker holding
            // the cookie value (from a log file, proxy, etc.) could still use
            // it on the next request.
            tracing::warn!(error = %err, "logout: session destroy failed");
        }
    }
    cookies.add(session_store::clear_cookie(&state.cfg.auth.session));
    Ok(Redirect::to("/login").into_response())
}

pub async fn logout_all(
    State(state): State<AppState>,
    cookies: Cookies,
    CurrentUser(user_id): CurrentUser,
) -> Result<axum::response::Response> {
    if let Some(pool) = state.db.as_ref() {
        session_store::destroy_all_for_user(pool, user_id).await?;
    }
    cookies.add(session_store::clear_cookie(&state.cfg.auth.session));
    Ok(Redirect::to("/login").into_response())
}
