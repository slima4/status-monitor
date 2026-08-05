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
//!   mints a fresh session cookie, and redirects: `/account/restore` for an
//!   account scheduled for deletion, else the joined org, else `/`.

use anyhow::Context;
use askama::Template;
use askama_web::WebTemplate;
use axum::Json;
use axum::extract::{Form, Query, State};
use axum::http::header::{CACHE_CONTROL, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tower_cookies::cookie::SameSite;
use tower_cookies::cookie::time::Duration as CookieDuration;
use tower_cookies::{Cookie, Cookies};

use crate::app::AppState;
use crate::auth::email_norm;
use crate::auth::login_audit::{self, LoginAttempt, LoginMethod};
use crate::auth::url::token_link;
use crate::auth::{fingerprint, magic_link, session as session_store, token_hash};
use crate::config::SessionConfig;
use crate::email::{EmailAddress, EmailTemplate, TransactionalEmail};
use crate::error::Result;
use crate::storage::orgs as orgs_store;
use crate::web::filters;

/// Name of the double-submit nonce cookie that ties the confirm-page GET to the
/// sign-in POST. Set on the GET, echoed in a hidden form field, checked on the
/// POST. A cross-site forged POST can neither read nor set it, so it can't
/// forge a login. Path-scoped to the verify endpoint; nothing else needs it.
const CONFIRM_NONCE_COOKIE: &str = "_sm_ml_confirm";

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

/// POST body of the confirmation form rendered by [`verify_landing`]. `token`
/// is the raw magic-link token echoed from the GET; `csrf` is the double-submit
/// nonce that must equal the [`CONFIRM_NONCE_COOKIE`] value.
#[derive(Debug, Deserialize)]
pub struct ConfirmForm {
    pub token: String,
    #[serde(default)]
    pub csrf: String,
}

/// Verify responses that aren't a redirect render a page, not JSON.
#[derive(Template, WebTemplate)]
#[template(path = "auth/magic_link_invalid.html")]
struct MagicLinkInvalidPage {
    expiry_minutes: u32,
    analytics: Option<&'static str>,
}

/// The one-click confirmation served on GET so a mail link-scanner's prefetch
/// can't burn the token. The token round-trips through a hidden field; the
/// human's button press is the POST that signs in.
#[derive(Template, WebTemplate)]
#[template(path = "auth/magic_link_confirm.html")]
struct MagicLinkConfirmPage {
    email: String,
    token: String,
    csrf: String,
    analytics: Option<&'static str>,
}

/// Failed redemptions render the indistinguishable invalid page. Status is
/// caller-chosen: `GONE` for a dead/used/expired token (never cacheable as
/// success, stays visible in status-code dashboards), `FORBIDDEN` for a
/// missing or mismatched confirmation nonce.
fn invalid_page(state: &AppState, status: StatusCode) -> Response {
    (
        status,
        MagicLinkInvalidPage {
            expiry_minutes: state.cfg.auth.magic_link.expiry_minutes,
            analytics: crate::analytics::website_id(&state.cfg.auth.public_base_url),
        },
    )
        .into_response()
}

/// Set the double-submit nonce cookie. Path-scoped to the verify endpoint,
/// HttpOnly (the hidden field is server-rendered, JS never reads it), and it
/// outlives the token so a still-valid link always has a live cookie to match.
fn set_confirm_nonce(cookies: &Cookies, cfg: &SessionConfig, nonce: &str, token_ttl_minutes: u32) {
    let mut c = Cookie::new(CONFIRM_NONCE_COOKIE, nonce.to_string());
    c.set_http_only(true);
    c.set_secure(cfg.cookie_secure);
    c.set_same_site(SameSite::Lax);
    c.set_path("/auth/magic-link/verify");
    if !cfg.cookie_domain.is_empty() {
        c.set_domain(cfg.cookie_domain.clone());
    }
    c.set_max_age(CookieDuration::minutes(i64::from(token_ttl_minutes) + 5));
    cookies.add(c);
}

/// Constant-time double-submit check: the posted `csrf` must be non-empty and
/// equal the nonce cookie the GET set on this browser.
fn confirm_nonce_ok(cookies: &Cookies, posted: &str) -> bool {
    let Some(cookie) = cookies.get(CONFIRM_NONCE_COOKIE) else {
        return false;
    };
    !posted.is_empty() && cookie.value().as_bytes().ct_eq(posted.as_bytes()).into()
}

/// Clear the nonce cookie once its confirmation has been spent.
fn clear_confirm_nonce(cookies: &Cookies, cfg: &SessionConfig) {
    let mut gone = Cookie::new(CONFIRM_NONCE_COOKIE, "");
    gone.set_path("/auth/magic-link/verify");
    if !cfg.cookie_domain.is_empty() {
        gone.set_domain(cfg.cookie_domain.clone());
    }
    cookies.remove(gone);
}

/// `GET /auth/magic-link/verify`. Read-only: peeks the token without consuming
/// it and, for a live token, renders a one-click confirmation page carrying a
/// fresh double-submit nonce. A mail scanner's prefetch lands here, leaves the
/// token intact, and the recipient's button press completes sign-in via POST.
pub async fn verify_landing(
    State(state): State<AppState>,
    Query(q): Query<VerifyQuery>,
    cookies: Cookies,
) -> Result<Response> {
    let pool = state.require_db()?;
    let Some(row) = magic_link::peek(pool, q.token.trim()).await? else {
        return Ok(invalid_page(&state, StatusCode::GONE));
    };
    let nonce = token_hash::generate_raw_token();
    set_confirm_nonce(
        &cookies,
        &state.cfg.auth.session,
        &nonce,
        state.cfg.auth.magic_link.expiry_minutes,
    );
    let mut resp = MagicLinkConfirmPage {
        email: row.email,
        token: q.token.trim().to_string(),
        csrf: nonce,
        analytics: crate::analytics::website_id(&state.cfg.auth.public_base_url),
    }
    .into_response();
    // Token-bearing page: never let an intermediary cache it.
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

/// `POST /auth/magic-link/verify`. Validates the double-submit nonce, then
/// atomically consumes the token, destroys any pre-login session bound to the
/// browser (fixation defence), creates a fresh session, and redirects by the
/// same priority as the OAuth callback.
pub async fn verify_confirm(
    State(state): State<AppState>,
    crate::web::client_ip::ClientIp(client_ip): crate::web::client_ip::ClientIp,
    headers: HeaderMap,
    cookies: Cookies,
    Form(form): Form<ConfirmForm>,
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

    if !confirm_nonce_ok(&cookies, &form.csrf) {
        login_audit::record_failure_anon(
            pool,
            LoginMethod::MagicLink,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            "confirm_csrf",
        )
        .await;
        return Ok(invalid_page(&state, StatusCode::FORBIDDEN));
    }

    let Some(row) = magic_link::consume(pool, form.token.trim()).await? else {
        login_audit::record_failure_anon(
            pool,
            LoginMethod::MagicLink,
            ip_hash.as_deref(),
            ua_hash.as_deref(),
            "invalid_token",
        )
        .await;
        return Ok(invalid_page(&state, StatusCode::GONE));
    };
    clear_confirm_nonce(&cookies, &state.cfg.auth.session);

    // Tombstone-inclusive: a verified link proves email ownership. Resolving
    // the row is not restoring it — same as the OAuth callbacks.
    let (user_id, pending_deletion, bootstrapped) =
        match orgs_store::find_user_by_email_including_deleted(pool, &row.email).await? {
            Some((user_id, deleted_at)) => {
                if let Some(deleted_at) = deleted_at {
                    tracing::info!(
                        user_id = %user_id.0,
                        deleted_at = %deleted_at,
                        "magic-link sign-in on an account scheduled for deletion; routing to the restore choice"
                    );
                }
                (user_id, deleted_at.is_some(), false)
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
                    return Ok(invalid_page(&state, StatusCode::GONE));
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
        return Ok(invalid_page(&state, StatusCode::GONE));
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

    // Invited bootstrap is the one path where a link mints an account.
    crate::analytics::track_login(
        &state,
        LoginMethod::MagicLink,
        bootstrapped,
        row.redirect_after.as_deref(),
        client_ip,
        &headers,
    );

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
            restored: false,
            invite_missed,
        },
        state.cfg.auth.session.cookie_secure,
        &state.cfg.auth.session.cookie_domain,
    );
    let redirect = if pending_deletion {
        crate::api::handlers::auth::RESTORE_PATH.to_string()
    } else if let Some(j) = joined {
        format!("/?joined={}", crate::auth::url::url_encode(&j.org_slug))
    } else if invite_missed {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_page_scrubs_the_token_from_every_analytics_send() {
        let html = MagicLinkConfirmPage {
            email: "who@example.com".into(),
            token: "s3cret-token".into(),
            csrf: "nonce".into(),
            analytics: Some("website-id"),
        }
        .render()
        .expect("renders");
        // The page URL holds a single-use credential.
        assert!(html.contains(r#"data-before-send="smScrubAuthUrl""#));
        assert!(html.contains("auth_url_scrub.js"));
        assert!(html.contains(r#"data-umami-event="magic-link-confirmed""#));
    }

    #[test]
    fn auth_pages_load_no_tracker_when_analytics_is_off() {
        let confirm = MagicLinkConfirmPage {
            email: "who@example.com".into(),
            token: "s3cret-token".into(),
            csrf: "nonce".into(),
            analytics: None,
        }
        .render()
        .expect("renders");
        assert!(!confirm.contains("analytics.uptimepage.dev"));

        let invalid = MagicLinkInvalidPage {
            expiry_minutes: 15,
            analytics: None,
        }
        .render()
        .expect("renders");
        assert!(!invalid.contains("analytics.uptimepage.dev"));
        assert!(!invalid.contains("auth_analytics.js"));
    }

    #[test]
    fn invalid_page_reports_the_failure_without_revealing_the_cause() {
        let html = MagicLinkInvalidPage {
            expiry_minutes: 15,
            analytics: Some("website-id"),
        }
        .render()
        .expect("renders");
        assert!(html.contains(r#"data-auth-event="login-error""#));
        assert!(html.contains(r#"data-auth-event-reason="link-invalid""#));
        assert!(html.contains("auth_analytics.js"));
        // A fixed reason cannot leak which of the three cases happened.
        assert_eq!(html.matches("data-auth-event-reason").count(), 1);
    }
}
