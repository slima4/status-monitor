use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;
use crate::notifier::truncate_chars;

/// Discord hard-caps message content at 2000 characters.
const MAX_CONTENT_CHARS: usize = 2000;

pub struct DiscordNotifier {
    client: OutboundHttpClient,
    webhook_url: Url,
}

#[derive(Serialize)]
struct DiscordPayload<'a> {
    content: &'a str,
}

impl DiscordNotifier {
    pub fn new(client: OutboundHttpClient, mut webhook_url: Url) -> Self {
        // Without `wait=true` Discord answers 204 before delivering, hiding
        // failures from the retry loop.
        webhook_url.query_pairs_mut().append_pair("wait", "true");
        Self {
            client,
            webhook_url,
        }
    }
}

#[async_trait]
impl Notifier for DiscordNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let text = truncate_chars(&notice.plain_text(), MAX_CONTENT_CHARS);
        post_json(
            &self.client,
            &self.webhook_url,
            &DiscordPayload { content: &text },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifier(url: &str) -> DiscordNotifier {
        DiscordNotifier::new(
            crate::http_outbound::build_outbound_client(
                crate::security::SsrfGuard::relaxed_for_tests(),
            ),
            url.parse().unwrap(),
        )
    }

    #[test]
    fn send_url_carries_wait_for_synchronous_errors() {
        let n = notifier("https://discord.com/api/webhooks/123/tok");
        assert_eq!(
            n.webhook_url.as_str(),
            "https://discord.com/api/webhooks/123/tok?wait=true"
        );
        let threaded = notifier("https://discord.com/api/webhooks/123/tok?thread_id=42");
        assert_eq!(
            threaded.webhook_url.as_str(),
            "https://discord.com/api/webhooks/123/tok?thread_id=42&wait=true"
        );
    }

    #[test]
    fn payload_is_plain_content() {
        let v = serde_json::to_value(DiscordPayload {
            content: "api-prod — major incident OPEN",
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "content": "api-prod — major incident OPEN" })
        );
    }
}
