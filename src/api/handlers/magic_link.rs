//! Magic-link sign-in endpoints. Both routes are mounted only when
//! `auth.enabled_methods` contains `"magic_link"`; otherwise the router never
//! exposes them and the surface is a 404.
//!
//! - `POST /auth/magic-link/request {email}` — anti-enumeration: the response
//!   shape is identical for every input, and the handler does the same
//!   argon2+INSERT work for every well-formed address regardless of whether a
//!   user owns it. Malformed inputs short-circuit (they disclose only that the
//!   string isn't an email shape — no signal about specific accounts). Email
//!   delivery and the per-email send throttle run inside `tokio::spawn` so
//!   neither SMTP latency nor rate-limit state leaks via response time. The
//!   throttle keeps a recipient's inbox to one link per
//!   `auth.magic_link.rate_limit_seconds` regardless of source.
//! - `GET  /auth/magic-link/verify?token=…` — atomically consumes the token,
//!   destroys any pre-login session bound to the browser (fixation defence),
//!   mints a fresh session cookie, and redirects to `/`.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::header::USER_AGENT;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::app::AppState;
use crate::auth::email_norm;
use crate::auth::login_audit::{self, LoginAttempt, LoginMethod};
use crate::auth::url::token_link;
use crate::auth::{fingerprint, magic_link, session as session_store};
use crate::email::{EmailAddress, EmailTemplate, TransactionalEmail};
use crate::error::Result;
use crate::storage::orgs as orgs_store;

#[derive(Debug, Deserialize)]
pub struct RequestBody {
    pub email: String,
}

#[derive(Debug, serde::Serialize)]
pub struct RequestResponse {
    /// Always `true`. The shape never branches on whether a user exists so
    /// the response is indistinguishable for enumeration probes.
    pub sent: bool,
}

/// `POST /auth/magic-link/request`. Anti-enumeration: returns the same shape
/// for every input and performs the same DB work for every well-formed email
/// regardless of whether a user owns it. Mail send is spawned so SMTP latency
/// doesn't make the response observable.
pub async fn request(
    State(state): State<AppState>,
    crate::web::client_ip::ClientIp(client_ip): crate::web::client_ip::ClientIp,
    Json(body): Json<RequestBody>,
) -> Result<Json<RequestResponse>> {
    let pool = state.require_db()?;
    let salt = state.cfg.auth.fingerprint_salt.as_str();
    let ip_hash = fingerprint::hash_fingerprint(salt, &client_ip.to_string());

    if let Some(email) = email_norm::normalize(&body.email) {
        let cfg = &state.cfg.auth.magic_link;
        // Always insert a row — including for emails with no user — so an
        // attacker cannot distinguish known vs unknown emails by timing. The
        // row simply never redeems because no user matches at /verify time.
        // Expires in 15 minutes (default); cleanup task drops the residue.
        let created =
            magic_link::create(pool, email, ip_hash.as_deref(), cfg.expiry_minutes).await?;
        let verify_url = token_link(
            &state.cfg.auth.public_base_url,
            "/auth/magic-link/verify",
            &created.token,
        );

        let outgoing = TransactionalEmail {
            from: EmailAddress::new(
                state.cfg.email.from_address.clone(),
                state.cfg.email.from_name.clone(),
            ),
            to: EmailAddress::new(email.to_string(), email.to_string()),
            template: EmailTemplate::MagicLink {
                url: verify_url,
                expires_in_minutes: cfg.expiry_minutes,
                ip_hint: Some(client_ip.to_string()),
            },
        };
        let sender = state.email_sender.clone();
        let throttle_pool = pool.clone();
        let throttle_email = email.to_string();
        let created_id = created.row.id;
        let rate_limit = cfg.rate_limit_seconds;
        // Spawn so SMTP duration doesn't leak via response time and so the
        // per-email throttle below also runs off the response path. The
        // handler returns the anti-enum response immediately.
        tokio::spawn(async move {
            // Per-email send throttle: only the first row inserted inside the
            // window for a given address actually mails the user. Later
            // duplicates still INSERT in the handler (anti-enum work) but the
            // throttle check here finds an earlier row and drops the send,
            // bounding the recipient's inbox to one link per window
            // regardless of how many sources hammer /request.
            //
            // Fail open on DB error — a transient hiccup must not silently
            // swallow a legitimate sign-in attempt.
            match magic_link::earliest_in_window(&throttle_pool, &throttle_email, rate_limit).await
            {
                Ok(Some(winner)) if winner != created_id => {
                    tracing::info!(
                        window_seconds = rate_limit,
                        "magic_link: rate-limited, suppressing email"
                    );
                    return;
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "magic_link: throttle check failed; sending anyway"
                    );
                }
            }
            if let Err(err) = sender.send(outgoing).await {
                tracing::warn!(error = %err, "magic_link: email send failed (background)");
            }
        });
    }

    Ok(Json(RequestResponse { sent: true }))
}

#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub token: String,
}

/// `GET /auth/magic-link/verify`. Atomically consumes the token, destroys any
/// pre-login session bound to the browser (fixation defence), creates a fresh
/// session, and redirects to `/`.
pub async fn verify(
    State(state): State<AppState>,
    Query(q): Query<VerifyQuery>,
    crate::web::client_ip::ClientIp(client_ip): crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Response> {
    let pool = state.require_db()?;
    let salt = state.cfg.auth.fingerprint_salt.as_str();
    let ip_hash = fingerprint::hash_fingerprint(salt, &client_ip.to_string());
    let ua_hash = fingerprint::hash_fingerprint(
        salt,
        headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
    );

    let Some(row) = magic_link::consume(pool, q.token.trim()).await? else {
        login_audit::record_failure_anon(
            pool,
            LoginMethod::MagicLink,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            "invalid_token",
        )
        .await;
        return Err(magic_link::invalid_token_error());
    };

    let Some(user_id) = orgs_store::find_user_by_email(pool, &row.email).await? else {
        // Token was consumed (marked used) but no user owns the email — either
        // the email never matched a user (anti-enum INSERT for an unknown
        // address) or the user was deleted between request and verify. The
        // token is burnt; surface the opaque error so a deleted-account probe
        // can't tell the difference from an invalid token.
        login_audit::record_failure_anon(
            pool,
            LoginMethod::MagicLink,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            "no_user_for_email",
        )
        .await;
        return Err(magic_link::invalid_token_error());
    };

    let cookie_name = state.cfg.auth.session.cookie_name.as_str();
    if let Some(prev) = cookies.get(cookie_name).map(|c| c.value().to_string())
        && !prev.is_empty()
        && let Err(err) = session_store::destroy(pool, &prev).await
    {
        tracing::warn!(error = %err, "magic_link: pre-login session destroy failed");
    }

    let created = session_store::create(
        pool,
        &state.cfg.auth.session,
        user_id,
        None,
        ip_hash.as_deref(),
        ua_hash.as_deref(),
    )
    .await?;

    if let Err(err) = login_audit::record(
        pool,
        LoginMethod::MagicLink,
        LoginAttempt {
            user_id: Some(user_id),
            success: true,
            ip_hash: ip_hash.as_deref(),
            user_agent_hash: ua_hash.as_deref(),
            failure_reason: None,
        },
    )
    .await
    {
        tracing::warn!(error = %err, "magic_link audit write failed (non-fatal)");
    }

    cookies.add(session_store::build_cookie(
        &state.cfg.auth.session,
        created.cookie_token,
    ));
    Ok(Redirect::to("/").into_response())
}
