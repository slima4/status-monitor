//! GitHub provider half of the OAuth login dance: authorize-URL builder and
//! Phase B (code exchange + profile/email fetch into a [`RemoteIdentity`]).
//! Phase A/C live in [`crate::auth::oauth_login`].

use http_body_util::Full;
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;

use crate::auth::oauth_login::{RemoteIdentity, UA, fetch_limited, parse_access_token};
use crate::auth::url::url_encode;
use crate::config::OauthClientConfig;
use crate::error::{AppError, Result};
use crate::http_outbound::OutboundHttpClient;

const GH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GH_USER_URL: &str = "https://api.github.com/user";
const GH_EMAILS_URL: &str = "https://api.github.com/user/emails";

/// Fallback when `[auth.github]` is partially set in TOML — a nested
/// `#[serde(default)]` section resets unlisted fields, emptying `scopes`.
const DEFAULT_SCOPES: &[&str] = &["user:email", "read:user"];

#[derive(Debug, Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
    email: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

/// Build the `https://github.com/login/oauth/authorize` URL with the configured
/// client id, scopes, redirect URI and state. The state must have already been
/// persisted to `oauth_states` before this URL is handed to the user.
pub fn authorize_url(cfg: &OauthClientConfig, state: &str) -> String {
    let scope = if cfg.scopes.is_empty() {
        DEFAULT_SCOPES.join(" ")
    } else {
        cfg.scopes.join(" ")
    };
    format!(
        "https://github.com/login/oauth/authorize?client_id={cid}&state={st}&scope={sc}&redirect_uri={ru}",
        cid = url_encode(&cfg.client_id),
        st = url_encode(state),
        sc = url_encode(&scope),
        ru = url_encode(&cfg.redirect_url),
    )
}

/// Phase B of the callback — exchange code, fetch profile + verified email.
/// Holds NO database connection across these three calls. `verified_email`
/// carries only addresses GitHub attests (profile email or verified primary).
pub async fn fetch_identity(
    http: &OutboundHttpClient,
    cfg: &OauthClientConfig,
    code: &str,
) -> Result<RemoteIdentity> {
    let token = exchange_code(http, cfg, code).await?;
    let user = fetch_user(http, &token).await?;
    let primary = fetch_primary_verified_email(http, &token)
        .await
        .ok()
        .flatten();
    let email = user.email.or(primary);
    Ok(RemoteIdentity {
        provider_user_id: user.id.to_string(),
        provider_username: Some(user.login),
        verified_email: email,
        display_name: user.name,
    })
}

async fn exchange_code(
    http: &OutboundHttpClient,
    cfg: &OauthClientConfig,
    code: &str,
) -> Result<String> {
    let payload = serde_json::to_vec(&json!({
        "client_id": cfg.client_id,
        "client_secret": cfg.client_secret.expose_secret(),
        "code": code,
        "redirect_uri": cfg.redirect_url,
    }))
    .map_err(|e| AppError::Other(anyhow::anyhow!("oauth token body: {e}")))?;
    let req = Request::post(GH_TOKEN_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, UA)
        .body(Full::new(Bytes::from(payload)))
        .map_err(|e| AppError::Other(anyhow::anyhow!("oauth token request: {e}")))?;
    let body = fetch_limited(http, req, "github token").await?;
    parse_access_token(&body, "github token endpoint")
}

async fn fetch_user(http: &OutboundHttpClient, access_token: &str) -> Result<GithubUser> {
    let req = Request::get(GH_USER_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| AppError::Other(anyhow::anyhow!("github user request: {e}")))?;
    let body = fetch_limited(http, req, "github user").await?;
    serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("github user parse: {e}")))
}

async fn fetch_primary_verified_email(
    http: &OutboundHttpClient,
    access_token: &str,
) -> Result<Option<String>> {
    let req = Request::get(GH_EMAILS_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| AppError::Other(anyhow::anyhow!("github emails request: {e}")))?;
    let body = fetch_limited(http, req, "github emails").await?;
    let emails: Vec<GithubEmail> = serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("github emails parse: {e}")))?;
    Ok(emails
        .into_iter()
        .find(|e| e.primary && e.verified)
        .map(|e| e.email))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_encodes_scope_and_redirect() {
        let cfg = OauthClientConfig {
            client_id: "cid".into(),
            redirect_url: "https://app.example.test/cb?next=/".into(),
            scopes: vec!["user:email".into(), "read:user".into()],
            ..Default::default()
        };
        let url = authorize_url(&cfg, "abc&def");
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("state=abc%26def"));
        // x-www-form-urlencoded encodes space as `+`, not %20.
        assert!(url.contains("scope=user%3Aemail+read%3Auser"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.example.test%2Fcb%3Fnext%3D%2F"));
    }

    #[test]
    fn authorize_url_falls_back_to_default_scopes_when_empty() {
        let cfg = OauthClientConfig {
            client_id: "cid".into(),
            redirect_url: "https://app.example.test/cb".into(),
            scopes: vec![],
            ..Default::default()
        };
        let url = authorize_url(&cfg, "s");
        assert!(url.contains("scope=user%3Aemail+read%3Auser"));
    }
}
