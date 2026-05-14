use async_trait::async_trait;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;

use super::trait_def::{EmailError, EmailResult, EmailSender, MessageId, TransactionalEmail};
use crate::http_outbound::OutboundHttpClient;

const RESEND_API_URL: &str = "https://api.resend.com/emails";
const MAX_RESEND_RESPONSE_BYTES: usize = 64 * 1024;

pub struct ResendEmailSender {
    api_key: SecretString,
    site_name: String,
    http: OutboundHttpClient,
}

impl ResendEmailSender {
    pub fn new(
        api_key: SecretString,
        site_name: impl Into<String>,
        http: OutboundHttpClient,
    ) -> Self {
        Self {
            api_key,
            site_name: site_name.into(),
            http,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ResendResponse {
    id: String,
}

#[async_trait]
impl EmailSender for ResendEmailSender {
    async fn send(&self, email: TransactionalEmail) -> EmailResult<MessageId> {
        let rendered = email.template.render(&self.site_name);
        let from_value = if email.from.name.is_empty() {
            email.from.address.clone()
        } else {
            format!("{} <{}>", email.from.name, email.from.address)
        };

        let payload = serde_json::to_vec(&json!({
            "from": from_value,
            "to": [email.to.address],
            "subject": rendered.subject,
            "text": rendered.text_body,
            "html": rendered.html_body,
        }))
        .map_err(|e| EmailError::Transport(format!("serialize: {e}")))?;

        let req = Request::post(RESEND_API_URL)
            .header(CONTENT_TYPE, "application/json")
            .header(
                AUTHORIZATION,
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .body(Full::new(Bytes::from(payload)))
            .map_err(|e| EmailError::Transport(format!("build request: {e}")))?;

        let resp = self
            .http
            .request(req)
            .await
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        let status = resp.status();
        let limited = Limited::new(resp.into_body(), MAX_RESEND_RESPONSE_BYTES);
        let body = limited
            .collect()
            .await
            .map_err(|e| EmailError::Transport(format!("read body: {e}")))?
            .to_bytes();

        if !status.is_success() {
            // Provider body may echo recipient/subject; surface enough for
            // operator triage without committing to a stable schema.
            let text = String::from_utf8_lossy(&body);
            return Err(EmailError::ProviderRejected(format!("{status}: {text}")));
        }

        let parsed: ResendResponse = serde_json::from_slice(&body)
            .map_err(|e| EmailError::Transport(format!("parse body: {e}")))?;
        Ok(MessageId(parsed.id))
    }
}
