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

use anyhow::Context;
use askama::Template;
use askama_web::WebTemplate;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, StatusCode};
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
use crate::web::filters;

#[derive(Debug, Deserialize)]
pub struct RequestBody {
    pub email: String,
    #[serde(default)]
    pub redirect_after: Option<String>,
    /// Raw invitation token; resolved to its row id like the OAuth starts.
    #[serde(default)]
    pub invitation: Option<String>,
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
        let redirect_after = body
            .redirect_after
            .as_deref()
            .and_then(crate::auth::url::safe_redirect_target);
        let invitation_id = crate::auth::invitations::resolve_pending_invitation_id(
            pool,
            body.invitation.as_deref(),
        )
        .await?;
        // Always insert — even for unknown emails — so timing can't
        // distinguish them; a no-user row never redeems at /verify.
        let created = magic_link::create(
            pool,
            email,
            ip_hash.as_deref(),
            cfg.expiry_minutes,
            redirect_after,
            invitation_id,
        )
        .await?;
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

/// /verify is a clicked-from-mail GET — failures get a page, not JSON.
#[derive(Template, WebTemplate)]
#[template(path = "auth/magic_link_invalid.html")]
struct MagicLinkInvalidPage {
    expiry_minutes: u32,
}

/// 410, not 200 — failed redemptions must stay visible in status-code
/// dashboards and never be cacheable as success.
fn invalid_page(state: &AppState) -> Response {
    (
        StatusCode::GONE,
        MagicLinkInvalidPage {
            expiry_minutes: state.cfg.auth.magic_link.expiry_minutes,
        },
    )
        .into_response()
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
        return Ok(invalid_page(&state));
    };

    // Tombstone-inclusive: a verified link proves email ownership, so
    // re-auth restores a soft-deleted account like the OAuth callbacks do.
    let (user_id, restored, bootstrapped) = match orgs_store::find_user_by_email_including_deleted(
        pool, &row.email,
    )
    .await?
    {
        Some((user_id, deleted_at)) => {
            let restored = deleted_at.is_some();
            if restored {
                let mut tx = pool.begin().await.context("magic-link restore: begin tx")?;
                crate::auth::account::undelete_in_tx(&mut tx, user_id).await?;
                tx.commit().await.context("magic-link restore: commit")?;
                tracing::info!(user_id = %user_id.0, "magic-link re-auth restored a soft-deleted account");
            }
            (user_id, restored, false)
        }
        // Unknown email: bootstrap an account ONLY for a valid carried
        // invitation whose address matches — the inviter's explicit
        // allowlist. Everything else stays the indistinguishable page.
        None => match bootstrap_invited_user(&state, &row).await? {
            Some((user_id, created)) => (user_id, false, created),
            None => {
                login_audit::record_failure_anon(
                    pool,
                    LoginMethod::MagicLink,
                    ip_hash.as_deref(),
                    ua_hash.as_deref(),
                    "no_user_for_email",
                )
                .await;
                return Ok(invalid_page(&state));
            }
        },
    };

    // Redeem a carried invitation before the session is minted so the
    // session opens in the joined org. Soft-fails — login never breaks.
    let joined = match row.invitation_id {
        Some(id) => crate::api::handlers::invitations::try_auto_accept(&state, user_id, id).await,
        None => None,
    };
    if bootstrapped && joined.is_none() {
        // The freshly minted user exists ONLY to redeem this invitation; a
        // raced revoke between pre-flight and accept must not leave an
        // org-less orphan account. No FK children yet — plain DELETE.
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id.0)
            .execute(pool)
            .await
            .context("magic-link bootstrap: compensating user delete")?;
        tracing::warn!(user_id = %user_id.0, "invited bootstrap raced a revoke/seat; user row compensated");
        login_audit::record_failure_anon(
            pool,
            LoginMethod::MagicLink,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            "invited_bootstrap_raced",
        )
        .await;
        return Ok(invalid_page(&state));
    }
    let active_org = match joined.as_ref().map(|j| j.org_id) {
        Some(org) => Some(org),
        // Resolving here also un-breaks plain magic logins: CurrentOrg
        // rejects a session whose active_org_id is NULL.
        None => crate::storage::users::resolve_signup_org(pool, user_id).await?,
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
        active_org,
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
    crate::web::login_hint::set(
        &cookies,
        &state.cfg.auth.session,
        LoginMethod::MagicLink.as_db_str(),
    );
    if let Err(err) = crate::web::display_prefs::issue_cookies(&state, &cookies, user_id).await {
        tracing::warn!(error = %err, "display-preference cookie issue failed (non-fatal)");
    }
    // One-shot banners ride a flash cookie (unspoofable, fires once); only the
    // slug-validated `joined` stays a query param. Same priority as the OAuth
    // callback.
    let invite_missed = joined.is_none() && row.invitation_id.is_some();
    crate::web::flash::set(
        &cookies,
        crate::web::flash::Flash {
            restored,
            invite_missed,
        },
        state.cfg.auth.session.cookie_secure,
        &state.cfg.auth.session.cookie_domain,
    );
    let redirect = if let Some(j) = joined {
        format!("/?joined={}", crate::auth::url::url_encode(&j.org_slug))
    } else if restored || invite_missed {
        "/".to_string()
    } else {
        row.redirect_after
            .as_deref()
            .and_then(crate::auth::url::safe_redirect_target)
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    };
    Ok(Redirect::to(&redirect).into_response())
}

/// B2 bootstrap gate. Returns the new user id only when the token row carries
/// a pending invitation whose address equals the token's email and whose org
/// can still take a member — checked BEFORE the user INSERT so failures
/// leave no orphan row. Every refusal collapses to `None` (anti-enum).
async fn bootstrap_invited_user(
    state: &AppState,
    row: &magic_link::MagicLinkRow,
) -> Result<Option<(crate::domain::UserId, bool)>> {
    let pool = state.require_db()?;
    let Some(invitation_id) = row.invitation_id else {
        return Ok(None);
    };
    let Some(invitation) =
        crate::auth::invitations::find_pending_by_id(pool, invitation_id).await?
    else {
        return Ok(None);
    };
    if !invitation.email.eq_ignore_ascii_case(&row.email) {
        return Ok(None);
    }
    if let Err(err) =
        crate::api::handlers::invitations::validate_acceptable(state, &invitation, None).await
    {
        tracing::warn!(error = %err, %invitation_id, "invited bootstrap pre-flight failed");
        return Ok(None);
    }
    let (user_id, created) = crate::storage::users::create_invited_user(pool, &row.email).await?;
    if created {
        tracing::info!(user_id = %user_id.0, via = "invitation", "magic-link bootstrap created account");
    }
    Ok(Some((user_id, created)))
}
