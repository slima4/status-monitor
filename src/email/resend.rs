use async_trait::async_trait;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;

use super::templates::single_line;
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

        let mut body = json!({
            "from": from_value,
            "to": [email.to.address],
            "subject": rendered.subject,
            "text": rendered.text_body,
            "html": rendered.html_body,
        });
        if let Some(addr) = email.template.reply_to() {
            body["reply_to"] = json!(single_line(addr));
        }
        let mut headers = serde_json::Map::new();
        // RFC 8058 one-click unsubscribe for public-subscriber mail: a stable
        // header link plus the One-Click marker so Gmail/Outlook render it.
        if let Some(url) = email.template.list_unsubscribe_url() {
            headers.insert("List-Unsubscribe".into(), json!(format!("<{url}>")));
            headers.insert(
                "List-Unsubscribe-Post".into(),
                json!("List-Unsubscribe=One-Click"),
            );
        }
        for (name, value) in email.template.custom_headers() {
            headers.insert(name.into(), json!(single_line(&value)));
        }
        if !headers.is_empty() {
            body["headers"] = serde_json::Value::Object(headers);
        }
        let payload = serde_json::to_vec(&body)
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
