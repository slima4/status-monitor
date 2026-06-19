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
    pub email: String,
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
        let page_name = page_display_name(pool, page.page.0).await;
        let confirm_url = token_link(
            &state.cfg.auth.public_base_url,
            "/subscribe/confirm",
            &token,
        );
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

async fn page_display_name(pool: &sqlx::PgPool, page_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(NULLIF(public_display_name, ''), name) FROM status_pages WHERE id = $1",
    )
    .bind(page_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "status page".to_string())
}
