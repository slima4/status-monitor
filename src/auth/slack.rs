//! Slack "Add to Slack" connect dance. The OAuth exchange returns a
//! ready-made incoming-webhook URL (Slack's consent screen carries the
//! channel picker); everything else in the token response — including the
//! access token — is discarded, so no Slack credential is ever stored.

use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::auth::url::url_encode;
use crate::config::ConnectOauthConfig;
use crate::error::{AppError, Result};
use crate::http_outbound::OutboundHttpClient;

const SLACK_AUTHORIZE_URL: &str = "https://slack.com/oauth/v2/authorize";
const SLACK_TOKEN_URL: &str = "https://slack.com/api/oauth.v2.access";
const SCOPE: &str = "incoming-webhook";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const UA: &str = "uptimepage/slack-connect";

/// Webhook minted by the consent screen; the only part of the token
/// response that survives.
#[derive(Debug, Clone, Deserialize)]
pub struct IncomingWebhook {
    pub url: String,
    /// Channel the user picked, e.g. `#ops-alerts`.
    pub channel: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    ok: bool,
    error: Option<String>,
    incoming_webhook: Option<IncomingWebhook>,
}

/// The state must already be persisted to `oauth_states` before this URL is
/// handed to the user.
pub fn authorize_url(cfg: &ConnectOauthConfig, redirect_uri: &str, state: &str) -> String {
    format!(
        "{SLACK_AUTHORIZE_URL}?client_id={cid}&scope={sc}&state={st}&redirect_uri={ru}",
        cid = url_encode(&cfg.client_id),
        sc = url_encode(SCOPE),
        st = url_encode(state),
        ru = url_encode(redirect_uri),
    )
}

/// Exchange the callback `code` at `oauth.v2.access` and keep only the
/// incoming webhook.
pub async fn exchange_code(
    http: &OutboundHttpClient,
    cfg: &ConnectOauthConfig,
    redirect_uri: &str,
    code: &str,
) -> Result<IncomingWebhook> {
    let payload = format!(
        "client_id={cid}&client_secret={cs}&code={code}&redirect_uri={ru}",
        cid = url_encode(&cfg.client_id),
        cs = url_encode(cfg.client_secret.expose_secret()),
        code = url_encode(code),
        ru = url_encode(redirect_uri),
    );
    let req = Request::post(SLACK_TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, UA)
        .body(Full::new(Bytes::from(payload)))
        .map_err(|e| AppError::Other(anyhow::anyhow!("slack token request: {e}")))?;
    let resp = http
        .request(req)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("slack request: {e}")))?;
    let status = resp.status();
    let body = Limited::new(resp.into_body(), MAX_RESPONSE_BYTES)
        .collect()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("slack body read: {e}")))?
        .to_bytes();
    if !status.is_success() {
        return Err(AppError::Other(anyhow::anyhow!(
            "slack token endpoint: http {status}"
        )));
    }
    let parsed: TokenResponse = serde_json::from_slice(&body)
        .map_err(|e| AppError::Other(anyhow::anyhow!("slack token parse: {e}")))?;
    if !parsed.ok {
        return Err(AppError::Other(anyhow::anyhow!(
            "slack token endpoint: {}",
            parsed.error.unwrap_or_else(|| "unknown error".into())
        )));
    }
    parsed.incoming_webhook.ok_or_else(|| {
        AppError::Other(anyhow::anyhow!(
            "slack token endpoint: ok response without incoming_webhook"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ConnectOauthConfig {
        ConnectOauthConfig {
            client_id: "123.456".into(),
            ..Default::default()
        }
    }

    #[test]
    fn authorize_url_encodes_state_and_redirect() {
        let url = authorize_url(
            &cfg(),
            "https://app.example.test/auth/slack/callback",
            "a&b",
        );
        assert!(url.starts_with("https://slack.com/oauth/v2/authorize?"));
        assert!(url.contains("client_id=123.456"));
        assert!(url.contains("scope=incoming-webhook"));
        assert!(url.contains("state=a%26b"));
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fapp.example.test%2Fauth%2Fslack%2Fcallback")
        );
    }

    #[test]
    fn token_response_keeps_webhook_and_ignores_access_token() {
        let parsed: TokenResponse = serde_json::from_str(
            r##"{"ok":true,"access_token":"xoxb-secret","incoming_webhook":
                {"url":"https://hooks.slack.com/services/T0/B0/XX","channel":"#ops",
                 "channel_id":"C0","configuration_url":"https://x.slack.com/services/B0"}}"##,
        )
        .unwrap();
        assert!(parsed.ok);
        let wh = parsed.incoming_webhook.unwrap();
        assert_eq!(wh.channel, "#ops");
        assert!(wh.url.starts_with("https://hooks.slack.com/"));
    }

    #[test]
    fn token_response_surfaces_slack_error() {
        let parsed: TokenResponse =
            serde_json::from_str(r#"{"ok":false,"error":"invalid_code"}"#).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.as_deref(), Some("invalid_code"));
    }
}
