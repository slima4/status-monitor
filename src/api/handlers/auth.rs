//! `/auth/*` endpoints: GitHub OAuth login + callback, logout.
//!
//! The callback follows a strict three-phase rule — no DB transaction held
//! across GitHub HTTP calls. New users get a signup org auto-created in the
//! same Phase C transaction that links their identity; the resolved default
//! org id is stamped onto the new session row so the next request lands on
//! a real org.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::header::USER_AGENT;
use axum::response::{IntoResponse, Redirect};
use secrecy::ExposeSecret;
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::api::error::codes;
use crate::app::AppState;
use crate::auth::{
    fingerprint, github,
    login_audit::{self, LoginAttempt, LoginMethod},
    oauth_state, session as session_store,
    url::safe_redirect_target,
};
use crate::error::{AppError, Result};
use crate::web::CurrentUser;

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub redirect_after: Option<String>,
    pub invitation: Option<String>,
}

pub async fn github_login(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> Result<Redirect> {
    let pool = state.db.as_ref().ok_or_else(|| {
        AppError::Other(anyhow::anyhow!(
            "github_login: no Postgres pool — auth requires tenancy mode"
        ))
    })?;
    let cfg = &state.cfg.auth.github;
    if cfg.client_id.is_empty() || cfg.client_secret.expose_secret().is_empty() {
        return Err(AppError::Other(anyhow::anyhow!(
            "github_login: auth.github.client_id/client_secret not configured"
        )));
    }
    // Only same-origin paths survive: anything else gets dropped to None so
    // the callback redirects to `/`. Without this, `?redirect_after=https://evil.test`
    // turns a legit OAuth dance into an open-redirect into attacker territory.
    let redirect_after = q.redirect_after.as_deref().and_then(safe_redirect_target);

    // Resolve the raw invitation token (if present) to its row id at this
    // edge instead of storing the token at rest in `oauth_states`. The id
    // alone isn't replayable — the accept handler still requires the
    // caller's session-bound email to match the invitation row.
    // Unknown / expired tokens fall through silently: the post-OAuth
    // redirect lands at `/`, the operator can re-issue the invite.
    let invitation_id = match q.invitation.as_deref() {
        Some(raw) if !raw.is_empty() => crate::auth::invitations::find_pending_by_token(pool, raw)
            .await?
            .map(|r| r.id),
        _ => None,
    };

    let s = oauth_state::generate_state();
    oauth_state::insert(
        pool,
        &s,
        crate::auth::OauthProvider::Github.as_db_str(),
        redirect_after,
        invitation_id,
    )
    .await?;
    let url = github::authorize_url(cfg, &s);
    Ok(Redirect::to(&url))
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
}

pub async fn github_callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
    crate::web::client_ip::ClientIp(client_ip): crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<axum::response::Response> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("github_callback: no Postgres pool")))?;
    let salt = state.cfg.auth.fingerprint_salt.as_str();
    let ip_hash = fingerprint::hash_fingerprint(salt, &client_ip.to_string());
    let ua_value = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let ua_hash = fingerprint::hash_fingerprint(salt, ua_value);

    // Phase A: consume state. Single-use; expired or unknown → 400.
    let Some(consumed) = oauth_state::consume(pool, &q.state).await? else {
        login_audit::record_failure_anon(
            pool,
            LoginMethod::GithubOauth,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            "invalid_state",
        )
        .await;
        return Err(AppError::bad_request(
            "INVALID_STATE",
            "OAuth state is invalid or has expired",
        ));
    };

    // Phase B: GitHub HTTP. NO DB connection held here.
    let identity =
        match github::fetch_identity(&state.outbound_http, &state.cfg.auth.github, &q.code).await {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!(error = %err, "github_callback: phase B failed");
                login_audit::record_failure_anon(
                    pool,
                    LoginMethod::GithubOauth,
                    ip_hash.as_deref(),
                    ua_hash.as_deref(),
                    "github_upstream_failed",
                )
                .await;
                return Err(AppError::Other(anyhow::anyhow!(
                    "github_callback: phase B: {err}"
                )));
            }
        };

    // Phase C: materialise user + identity, auto-create signup org for new
    // users, and resolve their default-org id for the session row. Fresh
    // transaction; no upstream calls.
    let resolved = github::upsert_identity_and_signup_org(pool, &identity)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "github_callback: phase C failed");
            e
        })?;
    if resolved.restored {
        tracing::info!(user_id = %resolved.user_id.0, "re-auth restored a soft-deleted account");
    }

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
        resolved.signup_org_id,
        ip_hash.as_deref(),
        ua_hash.as_deref(),
    )
    .await?;

    // Audit post-commit: a failure here logs but the session is already valid.
    if let Err(err) = login_audit::record(
        pool,
        LoginMethod::GithubOauth,
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
    if let Err(err) =
        crate::web::display_prefs::issue_cookies(&state, &cookies, resolved.user_id).await
    {
        tracing::warn!(error = %err, "display-preference cookie issue failed (non-fatal)");
    }

    let redirect = if resolved.is_new_user {
        "/onboarding/org".to_string()
    } else if let Some(invitation_id) = consumed.invitation_id {
        format!("/invitations/accept?invitation={invitation_id}")
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

// Silence dead-code on the imported error codes for the placeholder
// callbacks above — they reference the same `codes` module other handlers use
// for stable error codes.
#[allow(dead_code)]
const _: &str = codes::UNAUTHORIZED;
