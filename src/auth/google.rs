//! Google provider half of the OAuth login dance: authorize-URL builder and
//! Phase B (code exchange + OIDC userinfo fetch into a [`RemoteIdentity`]).
//! Phase A/C live in [`crate::auth::oauth_login`].
//!
//! Userinfo over the TLS channel replaces id_token signature verification —
//! the bytes come straight from Google, so a JWKS dependency buys nothing.

use http_body_util::Full;
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::auth::oauth_login::{RemoteIdentity, UA, fetch_limited, parse_access_token};
use crate::auth::url::url_encode;
use crate::config::OauthClientConfig;
use crate::error::{AppError, Result};
use crate::http_outbound::OutboundHttpClient;

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// Fallback when `[auth.google]` is partially set in TOML — a nested
/// `#[serde(default)]` section resets unlisted fields, emptying `scopes`.
const DEFAULT_SCOPES: &[&str] = &["openid", "email", "profile"];

#[derive(Debug, Deserialize)]
struct UserInfo {
    sub: String,
    email: Option<String>,
    #[serde(default, deserialize_with = "de_bool_loose")]
    email_verified: Option<bool>,
    name: Option<String>,
}

/// Legacy Google surfaces emit `email_verified` as the string "true"; a
/// strict bool would fail the whole parse and lock out sub-match logins.
fn de_bool_loose<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        B(bool),
        S(String),
    }
    Ok(match Option::<Loose>::deserialize(d)? {
        Some(Loose::B(b)) => Some(b),
        Some(Loose::S(s)) if s.eq_ignore_ascii_case("true") => Some(true),
        Some(Loose::S(_)) | None => None,
    })
}

fn map_userinfo(info: UserInfo) -> RemoteIdentity {
    let verified_email = info
        .email
        .clone()
        .filter(|_| info.email_verified == Some(true));
    RemoteIdentity {
        provider_user_id: info.sub,
        provider_username: info.email,
        verified_email,
        display_name: info.name,
    }
}

pub fn authorize_url(cfg: &OauthClientConfig, state: &str) -> String {
    let scope = if cfg.scopes.is_empty() {
        DEFAULT_SCOPES.join(" ")
    } else {
        cfg.scopes.join(" ")
    };
    format!(
        "{GOOGLE_AUTH_URL}?response_type=code&client_id={cid}&state={st}&scope={sc}&redirect_uri={ru}",
        cid = url_encode(&cfg.client_id),
        st = url_encode(state),
        sc = url_encode(&scope),
        ru = url_encode(&cfg.redirect_url),
    )
}

/// Phase B — exchange code, fetch the OIDC userinfo. No DB connection held.
/// `verified_email` stays None unless Google attests `email_verified` — an
/// unverified address must never reach the email-link path (account takeover).
pub async fn fetch_identity(
    http: &OutboundHttpClient,
    cfg: &OauthClientConfig,
    code: &str,
) -> Result<RemoteIdentity> {
    let token = exchange_code(http, cfg, code).await?;
    let info = fetch_userinfo(http, &token).await?;
    Ok(map_userinfo(info))
}

async fn exchange_code(
    http: &OutboundHttpClient,
    cfg: &OauthClientConfig,
    code: &str,
) -> Result<String> {
    // Google's token endpoint rejects JSON bodies — form-urlencoded only.
    let payload = crate::auth::url::form_body(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", &cfg.client_id),
        ("client_secret", cfg.client_secret.expose_secret()),
        ("redirect_uri", &cfg.redirect_url),
    ]);
    let req = Request::post(GOOGLE_TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, UA)
        .body(Full::new(Bytes::from(payload)))
        .map_err(|e| AppError::Other(anyhow::anyhow!("google token request: {e}")))?;
    let body = fetch_limited(http, req, "google token").await?;
    parse_access_token(&body, "google token endpoint")
}

async fn fetch_userinfo(http: &OutboundHttpClient, access_token: &str) -> Result<UserInfo> {
    let req = Request::get(GOOGLE_USERINFO_URL)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| AppError::Other(anyhow::anyhow!("google userinfo request: {e}")))?;
    let body = fetch_limited(http, req, "google userinfo").await?;
    serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("google userinfo parse: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_has_code_response_type_and_encoded_scopes() {
        let cfg = OauthClientConfig {
            client_id: "cid".into(),
            redirect_url: "https://app.example.test/auth/google/callback".into(),
            scopes: vec![],
            ..Default::default()
        };
        let url = authorize_url(&cfg, "st&ate");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?response_type=code"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("state=st%26ate"));
        assert!(url.contains("scope=openid+email+profile"));
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fapp.example.test%2Fauth%2Fgoogle%2Fcallback")
        );
    }

    fn identity_from(json: &str) -> RemoteIdentity {
        map_userinfo(serde_json::from_str(json).unwrap())
    }

    #[test]
    fn unverified_or_missing_email_never_links() {
        let id =
            identity_from(r#"{"sub":"123","email":"a@b.test","email_verified":false,"name":"A"}"#);
        assert_eq!(id.verified_email, None);
        assert_eq!(id.provider_username.as_deref(), Some("a@b.test"));

        let id = identity_from(r#"{"sub":"123","email":"a@b.test","name":"A"}"#);
        assert_eq!(id.verified_email, None);

        let id =
            identity_from(r#"{"sub":"123","email":"a@b.test","email_verified":true,"name":"A"}"#);
        assert_eq!(id.verified_email.as_deref(), Some("a@b.test"));
    }

    #[test]
    fn legacy_string_email_verified_parses_instead_of_breaking_login() {
        // String "true" → verified; any other string → unverified, but the
        // parse itself must succeed so sub-match logins keep working.
        let id =
            identity_from(r#"{"sub":"123","email":"a@b.test","email_verified":"true","name":"A"}"#);
        assert_eq!(id.verified_email.as_deref(), Some("a@b.test"));

        let id = identity_from(
            r#"{"sub":"123","email":"a@b.test","email_verified":"false","name":"A"}"#,
        );
        assert_eq!(id.verified_email, None);
        assert_eq!(id.provider_user_id, "123");
    }
}
