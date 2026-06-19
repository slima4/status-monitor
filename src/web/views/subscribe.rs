//! Public status-page subscriptions (double opt-in email). Chrome-less,
//! unauthenticated: possession of the mailed confirm token, or of a valid
//! unsubscribe HMAC, is the proof. `POST /subscribe` always renders the same
//! "check your inbox" notice whether the address is new, already subscribed,
//! or rate-limited, so the surface leaks no membership signal.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::email_norm;
use crate::auth::url::token_link;
use crate::domain::{NewSubscriber, SubscriberChannel};
use crate::email::{EmailAddress, EmailTemplate, TransactionalEmail};
use crate::http_outbound::post_bytes_with_headers;
use crate::storage::subscribers::{self, CONFIRM_TTL_HOURS};
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::host::resolve_status_page;

#[derive(Template, WebTemplate)]
#[template(path = "public/subscribe_notice.html")]
pub struct SubscribeNotice {
    pub ok: bool,
    pub heading: String,
    pub message: String,
}

impl SubscribeNotice {
    fn ok(heading: &str, message: &str) -> Response {
        SubscribeNotice {
            ok: true,
            heading: heading.into(),
            message: message.into(),
        }
        .into_response()
    }

    fn bad(status: StatusCode, heading: &str, message: &str) -> Response {
        (
            status,
            SubscribeNotice {
                ok: false,
                heading: heading.into(),
                message: message.into(),
            },
        )
            .into_response()
    }
}

#[derive(Debug, Deserialize)]
pub struct SubscribeForm {
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub url: String,
}

pub async fn subscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SubscribeForm>,
) -> WebResult<Response> {
    let invalid_page = || {
        SubscribeNotice::bad(
            StatusCode::NOT_FOUND,
            "Page not found",
            "This status page isn't available.",
        )
    };

    let page = match resolve_status_page(&state, &headers).await {
        Ok(p) => p,
        Err(_) => return Ok(invalid_page()),
    };
    let Some(pool) = state.db.as_ref() else {
        return Ok(invalid_page());
    };

    if form.channel == "webhook" {
        return subscribe_webhook(&state, pool, page, form.url.trim()).await;
    }

    let Some(email) = email_norm::normalize(&form.email) else {
        return Ok(SubscribeNotice::bad(
            StatusCode::BAD_REQUEST,
            "Check the address",
            "That doesn't look like a valid email address.",
        ));
    };
    // Lowercase so the unique index folds case variants into one subscription.
    let email = email.to_ascii_lowercase();

    let sub = subscribers::subscribe(
        pool,
        &NewSubscriber {
            status_page_id: page.page.0,
            org_id: page.org.0,
            channel: SubscriberChannel::Email,
            target: email.to_string(),
            config: serde_json::json!({}),
        },
    )
    .await?;

    if let subscribers::ConfirmMint::Created { token } =
        subscribers::mint_confirm_token(pool, sub.id, page.org.0, page.page.0, &sub.target).await?
    {
        let meta = page_meta(pool, page.page.0).await;
        let origin = match &meta {
            Some(m) => crate::web::host::page_origin(
                &state.cfg.public_status.base_domain,
                &state.cfg.auth.public_base_url,
                &m.slug,
                m.custom_domain.as_deref(),
                m.custom_domain_verified,
            ),
            None => state
                .cfg
                .auth
                .public_base_url
                .trim_end_matches('/')
                .to_string(),
        };
        let page_name = meta.map_or_else(|| "status page".to_string(), |m| m.name);
        let confirm_url = token_link(&origin, "/subscribe/confirm", &token);
        let outgoing = TransactionalEmail {
            from: EmailAddress::new(
                state.cfg.email.from_address.clone(),
                state.cfg.email.from_name.clone(),
            ),
            to: EmailAddress::new(sub.target.clone(), sub.target.clone()),
            template: EmailTemplate::SubscriberConfirm {
                page_name,
                confirm_url,
                expires_hours: CONFIRM_TTL_HOURS as u32,
            },
        };
        if let Err(err) = state.email_sender.send(outgoing).await {
            tracing::warn!(error = %err, "subscriber confirm send failed");
        }
    }

    Ok(SubscribeNotice::ok(
        "Almost there",
        "Check your inbox for a confirmation link to finish subscribing.",
    ))
}

#[derive(Debug, Deserialize)]
pub struct ConfirmQuery {
    #[serde(default)]
    pub token: String,
}

pub async fn confirm(
    State(state): State<AppState>,
    Query(q): Query<ConfirmQuery>,
) -> WebResult<Response> {
    let invalid = || {
        SubscribeNotice::bad(
            StatusCode::NOT_FOUND,
            "Link expired",
            "This confirmation link is invalid, expired, or already used.",
        )
    };
    let token = q.token.trim();
    if token.is_empty() {
        return Ok(invalid());
    }
    let Some(pool) = state.db.as_ref() else {
        return Ok(invalid());
    };
    match subscribers::confirm(pool, token).await? {
        Some(_) => Ok(SubscribeNotice::ok(
            "You're subscribed",
            "You'll be notified when the status of this page changes.",
        )),
        None => Ok(invalid()),
    }
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeQuery {
    #[serde(default)]
    pub s: String,
    #[serde(default)]
    pub t: String,
}

pub async fn unsubscribe(
    State(state): State<AppState>,
    Query(q): Query<UnsubscribeQuery>,
) -> WebResult<Response> {
    let invalid = || {
        SubscribeNotice::bad(
            StatusCode::NOT_FOUND,
            "Invalid link",
            "This unsubscribe link is invalid.",
        )
    };
    let Ok(id) = Uuid::parse_str(q.s.trim()) else {
        return Ok(invalid());
    };
    if !subscribers::verify_unsubscribe(&state.subscription_unsubscribe_secret, id, q.t.trim()) {
        return Ok(invalid());
    }
    let Some(pool) = state.db.as_ref() else {
        return Ok(invalid());
    };
    subscribers::unsubscribe(pool, id).await?;
    Ok(SubscribeNotice::ok(
        "Unsubscribed",
        "You won't receive any more updates from this status page.",
    ))
}

/// Webhook subscription: no inbox to mail a token to, so a live verification
/// POST (through the SSRF-guarded outbound client) proves the endpoint is
/// reachable and the subscriber controls it, and the row is verified at once.
async fn subscribe_webhook(
    state: &AppState,
    pool: &sqlx::PgPool,
    page: crate::domain::status_page::PageRef,
    url: &str,
) -> WebResult<Response> {
    let bad_url = || {
        SubscribeNotice::bad(
            StatusCode::BAD_REQUEST,
            "Check the URL",
            "Enter a valid https:// webhook URL.",
        )
    };
    let Ok(parsed) = url::Url::parse(url) else {
        return Ok(bad_url());
    };
    if parsed.scheme() != "https" {
        return Ok(bad_url());
    }
    // Per-subscriber signing secret: the receiver verifies our
    // X-Uptimepage-Signature with it. Shown once on success.
    let secret = crate::auth::token_hash::generate_raw_token();
    let ping = serde_json::json!({
        "type": "subscription_verification",
        "message": "Confirm your subscription to status updates.",
    });
    let Ok(body) = serde_json::to_vec(&ping) else {
        return Ok(bad_url());
    };
    let ts = chrono::Utc::now().timestamp();
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("X-Uptimepage-Timestamp".to_string(), ts.to_string());
    headers.insert(
        "X-Uptimepage-Signature".to_string(),
        crate::auth::mac::webhook_signature(&secret, ts, &body),
    );
    if post_bytes_with_headers(&state.outbound_http, &parsed, body, &headers)
        .await
        .is_err()
    {
        return Ok(SubscribeNotice::bad(
            StatusCode::BAD_REQUEST,
            "Couldn't reach your endpoint",
            "We couldn't deliver a verification POST to that URL. Make sure it accepts HTTPS POST and returns 2xx, then try again.",
        ));
    }
    let sub = subscribers::subscribe(
        pool,
        &NewSubscriber {
            status_page_id: page.page.0,
            org_id: page.org.0,
            channel: SubscriberChannel::Webhook,
            target: url.to_string(),
            config: serde_json::json!({ "signing_secret": secret }),
        },
    )
    .await?;
    subscribers::mark_verified(pool, sub.id).await?;
    // Re-subscribe keeps the original secret (the upsert leaves config), so
    // show whatever is actually stored.
    let shown = sub
        .config
        .get("signing_secret")
        .and_then(|v| v.as_str())
        .unwrap_or(&secret);
    Ok(SubscribeNotice::ok(
        "Subscribed",
        &format!(
            "Your endpoint is verified. Save this signing secret to verify our requests: {shown}"
        ),
    ))
}

struct PageMeta {
    name: String,
    slug: String,
    custom_domain: Option<String>,
    custom_domain_verified: bool,
}

async fn page_meta(pool: &sqlx::PgPool, page_id: Uuid) -> Option<PageMeta> {
    let row: (String, String, Option<String>, bool) = sqlx::query_as(
        "SELECT COALESCE(NULLIF(public_display_name, ''), name), slug::text,
                custom_domain::text, custom_domain_verified_at IS NOT NULL
         FROM status_pages WHERE id = $1",
    )
    .bind(page_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    Some(PageMeta {
        name: row.0,
        slug: row.1,
        custom_domain: row.2,
        custom_domain_verified: row.3,
    })
}
