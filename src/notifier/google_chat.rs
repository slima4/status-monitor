use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;
use crate::notifier::truncate_chars;

/// Google Chat caps message text at 4096 characters.
const MAX_TEXT_CHARS: usize = 4096;

pub struct GoogleChatNotifier {
    client: OutboundHttpClient,
    webhook_url: Url,
}

#[derive(Serialize)]
struct GoogleChatPayload<'a> {
    text: &'a str,
}

impl GoogleChatNotifier {
    pub fn new(client: OutboundHttpClient, webhook_url: Url) -> Self {
        Self {
            client,
            webhook_url,
        }
    }
}

#[async_trait]
impl Notifier for GoogleChatNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let text = truncate_chars(&notice.plain_text(), MAX_TEXT_CHARS);
        post_json(
            &self.client,
            &self.webhook_url,
            &GoogleChatPayload { text: &text },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_plain_text() {
        let v = serde_json::to_value(GoogleChatPayload {
            text: "api-prod — incident RESOLVED after 5m",
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "text": "api-prod — incident RESOLVED after 5m" })
        );
    }
}
