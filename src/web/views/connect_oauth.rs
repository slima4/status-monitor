//! Provider-parameterized pieces shared by the OAuth connect flows
//! (`slack_connect`, `discord_connect`, and the delegate starts).

use axum::Json;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::provider::{DISCORD_CONNECT_PROVIDER, SLACK_CONNECT_PROVIDER};
use crate::auth::{discord, oauth_state, slack};
use crate::config::{AppConfig, ConnectOauthConfig};
use crate::domain::OrgId;
use crate::error::{AppError, Result};

/// Everything that distinguishes one OAuth connect provider from another.
pub struct ConnectProvider {
    /// Path segment of the callback route, delegate `kind_hint`, and the
    /// bounce query key.
    pub kind: &'static str,
    /// `oauth_states.provider` value the starts mint.
    pub state_provider: &'static str,
    pub cfg: fn(&AppConfig) -> &ConnectOauthConfig,
    pub authorize_url: fn(&ConnectOauthConfig, &str, &str) -> String,
}

pub const SLACK: ConnectProvider = ConnectProvider {
    kind: "slack",
    state_provider: SLACK_CONNECT_PROVIDER,
    cfg: |c| &c.slack_oauth,
    authorize_url: slack::authorize_url,
};

pub const DISCORD: ConnectProvider = ConnectProvider {
    kind: "discord",
    state_provider: DISCORD_CONNECT_PROVIDER,
    cfg: |c| &c.discord_oauth,
    authorize_url: discord::authorize_url,
};

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    /// `json` returns `{ "url": … }` for the QR variant instead of a 302.
    pub format: Option<String>,
}

impl StartQuery {
    pub fn wants_json(&self) -> bool {
        self.format.as_deref() == Some("json")
    }
}

/// `?<kind>=<outcome>` appended to the bounce target, which already carries
/// a query string on the dashboard form but not on a `/c/<code>` page.
pub fn bounce(p: &ConnectProvider, base: &str, outcome: &str) -> Response {
    let sep = if base.contains('?') { '&' } else { '?' };
    Redirect::to(&format!("{base}{sep}{}={outcome}", p.kind)).into_response()
}

pub fn callback_uri(state: &AppState, p: &ConnectProvider) -> String {
    format!(
        "{}/auth/{}/callback",
        state.cfg.auth.public_base_url.trim_end_matches('/'),
        p.kind
    )
}

pub fn invalid_state() -> AppError {
    AppError::forbidden_code("INVALID_STATE", "OAuth state is invalid or has expired")
}

/// Mints an `oauth_states` row bound to `org` (and, for delegate starts,
/// the link), then answers with the provider's authorize URL.
pub async fn mint_start_response(
    state: &AppState,
    p: &ConnectProvider,
    wants_json: bool,
    org: OrgId,
    link_code_id: Option<Uuid>,
) -> Result<Response> {
    let pool = state.require_db()?;
    let s = oauth_state::generate_state();
    oauth_state::insert(
        pool,
        &s,
        p.state_provider,
        None,
        None,
        Some(org.0),
        link_code_id,
    )
    .await?;
    let url = (p.authorize_url)((p.cfg)(&state.cfg), &callback_uri(state, p), &s);
    Ok(if wants_json {
        Json(json!({ "url": url })).into_response()
    } else {
        Redirect::to(&url).into_response()
    })
}
