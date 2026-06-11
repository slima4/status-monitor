//! "Add to Discord" connect flow (`/auth/discord/start` +
//! `/auth/discord/callback`). Same shape as the Slack flow in
//! `slack_connect`, including the two callback authorities: a
//! session-minted state (membership re-checked against the state org) or a
//! delegation-link state from `/c/<code>/discord/start`, spent by the
//! successful create. Only the webhook survives the token exchange, stored
//! as a regular `discord` channel.

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::provider::DISCORD_CONNECT_PROVIDER;
use crate::auth::{discord, oauth_state};
use crate::domain::{ChannelConfig, DiscordConfig, OrgId};
use crate::error::{AppError, Result};
use crate::storage::orgs::is_active_member;
use crate::web::client_ip::ClientIp;
use crate::web::views::delegate_connect::{audit_delegated_create, finish_create};
use crate::web::views::notification_channels::{QuotaBlockLog, create_channel_deduped};
use crate::web::{Authorized, ChannelsWrite, CurrentUser};

const FORM_URL: &str = "/settings/notifications/new?kind=discord";

/// `?discord=<outcome>` appended to the bounce target, which already
/// carries a query string on the dashboard form but not on a `/c/<code>`
/// page.
fn bounce(base: &str, outcome: &str) -> Response {
    let sep = if base.contains('?') { '&' } else { '?' };
    Redirect::to(&format!("{base}{sep}discord={outcome}")).into_response()
}

fn redirect_uri(state: &AppState) -> String {
    format!(
        "{}/auth/discord/callback",
        state.cfg.auth.public_base_url.trim_end_matches('/')
    )
}

fn invalid_state() -> AppError {
    AppError::forbidden_code("INVALID_STATE", "OAuth state is invalid or has expired")
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    /// `json` returns `{ "url": … }` for the QR variant instead of a 302.
    pub format: Option<String>,
}

pub async fn start(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<ChannelsWrite>,
    Query(q): Query<StartQuery>,
) -> Result<Response> {
    let pool = state.require_db()?;
    let s = oauth_state::generate_state();
    oauth_state::insert(
        pool,
        &s,
        DISCORD_CONNECT_PROVIDER,
        None,
        None,
        Some(org.0),
        None,
    )
    .await?;
    let url = discord::authorize_url(&state.cfg.discord_oauth, &redirect_uri(&state), &s);
    Ok(if q.format.as_deref() == Some("json") {
        Json(serde_json::json!({ "url": url })).into_response()
    } else {
        Redirect::to(&url).into_response()
    })
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

pub async fn callback(
    State(state): State<AppState>,
    user: Result<CurrentUser, AppError>,
    ClientIp(client_ip): ClientIp,
    Query(q): Query<CallbackQuery>,
) -> Result<Response> {
    let pool = state.require_db()?;
    let raw_state = q.state.as_deref().unwrap_or_default();
    if raw_state.is_empty() {
        return Err(invalid_state());
    }
    // Consume FIRST — also on the user-cancelled path, so a denied dance
    // burns its state.
    let Some(consumed) = oauth_state::consume(pool, raw_state).await? else {
        tracing::info!(
            reason = "state_unknown_or_expired",
            "discord connect rejected"
        );
        return Err(invalid_state());
    };
    if consumed.provider != DISCORD_CONNECT_PROVIDER {
        tracing::info!(reason = "provider_mismatch", "discord connect rejected");
        return Err(invalid_state());
    }
    let Some(org) = consumed.org_id.map(OrgId) else {
        tracing::warn!(reason = "state_without_org", "discord connect rejected");
        return Err(invalid_state());
    };
    // Authority: a delegate-minted state rides the link's one-channel
    // budget; a dashboard-minted state needs the session user to still be
    // an active member of the state org.
    let session_user = if consumed.link_code_id.is_none() {
        let CurrentUser(user_id) = user?;
        if !is_active_member(pool, user_id, org).await? {
            tracing::info!(org_id = %org.0, reason = "membership_lost", "discord connect rejected");
            return Err(AppError::Forbidden);
        }
        Some(user_id)
    } else {
        None
    };
    let bounce_base =
        consumed
            .redirect_after
            .as_deref()
            .unwrap_or(if consumed.link_code_id.is_some() {
                "/c/done"
            } else {
                FORM_URL
            });
    if let Some(err) = q.error.as_deref().filter(|e| !e.is_empty()) {
        tracing::info!(org_id = %org.0, reason = err, "discord connect cancelled");
        return Ok(bounce(bounce_base, "cancelled"));
    }
    let Some(code) = q.code.as_deref().filter(|c| !c.is_empty()) else {
        return Err(AppError::bad_request(
            "INVALID_STATE",
            "OAuth callback carried neither code nor error",
        ));
    };
    // Spend the delegate link before the upstream exchange; every failure
    // path below restores it so a transient error doesn't burn the invite.
    let delegate = match consumed.link_code_id {
        Some(link_id) => match state.channel_link_code_store.consume_by_id(link_id).await? {
            Some(link) => Some(link),
            None => {
                tracing::info!(reason = "delegate_link_dead", "discord connect rejected");
                return Err(invalid_state());
            }
        },
        None => None,
    };

    let webhook = match discord::exchange_code(
        &state.outbound_http,
        &state.cfg.discord_oauth,
        &redirect_uri(&state),
        code,
    )
    .await
    {
        Ok(wh) => wh,
        Err(err) => {
            tracing::warn!(org_id = %org.0, error = %err, "discord connect exchange failed");
            if let Some(link) = &delegate {
                state.channel_link_code_store.restore(link.id).await?;
            }
            return Ok(bounce(bounce_base, "failed"));
        }
    };
    let config = ChannelConfig::Discord(DiscordConfig {
        webhook_url: webhook.url,
    });
    // Discord minted the URL, but the shared transport validation still
    // runs — a malformed value must not enter the store through this side
    // door.
    if let Err(err) = config.validate() {
        tracing::warn!(org_id = %org.0, error = %err, "discord connect returned an invalid webhook");
        if let Some(link) = &delegate {
            state.channel_link_code_store.restore(link.id).await?;
        }
        return Ok(bounce(bounce_base, "failed"));
    }

    let base_name = webhook
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Discord")
        .to_string();
    if let Some(link) = &delegate {
        let base_name = link.channel_name.clone().unwrap_or(base_name);
        return match finish_create(&state, link, &base_name, config, "discord_oauth").await {
            Ok(ch) => {
                audit_delegated_create(&state, org, &ch, &client_ip.to_string()).await;
                Ok(Redirect::to("/c/done").into_response())
            }
            Err(AppError::Unprocessable { code, .. })
                if code == crate::api::error::codes::CHANNEL_QUOTA_EXCEEDED =>
            {
                state.channel_link_code_store.restore(link.id).await?;
                Ok(bounce(bounce_base, "quota"))
            }
            Err(err) => {
                state.channel_link_code_store.restore(link.id).await?;
                Err(err)
            }
        };
    }

    let limit = i64::from(
        state
            .quotas
            .limit_for_org(org)
            .await?
            .max_notification_channels,
    );
    match create_channel_deduped(
        state.notification_channel_store.as_ref(),
        org,
        &base_name,
        config,
        None,
        limit,
        QuotaBlockLog {
            db: state.db.clone(),
            user: session_user,
            flow: "discord_oauth",
        },
    )
    .await
    {
        Ok(ch) => {
            tracing::info!(org_id = %org.0, channel_id = %ch.id, "discord channel connected");
            Ok(Redirect::to(&format!("/settings/notifications/{}/edit", ch.id)).into_response())
        }
        Err(AppError::Unprocessable { code, .. })
            if code == crate::api::error::codes::CHANNEL_QUOTA_EXCEEDED =>
        {
            tracing::info!(org_id = %org.0, reason = "quota", "discord connect rejected");
            Ok(bounce(bounce_base, "quota"))
        }
        Err(err) => Err(err),
    }
}
