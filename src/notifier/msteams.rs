use async_trait::async_trait;
use serde::Serialize;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json};
use crate::notifier::Notifier;
use crate::notifier::event::IncidentNotice;

pub struct MsTeamsNotifier {
    client: OutboundHttpClient,
    webhook_url: Url,
}

#[derive(Serialize)]
struct TeamsMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    attachments: [Attachment<'a>; 1],
}

#[derive(Serialize)]
struct Attachment<'a> {
    #[serde(rename = "contentType")]
    content_type: &'static str,
    content: AdaptiveCard<'a>,
}

#[derive(Serialize)]
struct AdaptiveCard<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    version: &'static str,
    body: [TextBlock<'a>; 1],
}

#[derive(Serialize)]
struct TextBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    wrap: bool,
}

impl MsTeamsNotifier {
    pub fn new(client: OutboundHttpClient, webhook_url: Url) -> Self {
        Self {
            client,
            webhook_url,
        }
    }

    fn message<'a>(text: &'a str) -> TeamsMessage<'a> {
        TeamsMessage {
            kind: "message",
            attachments: [Attachment {
                content_type: "application/vnd.microsoft.card.adaptive",
                content: AdaptiveCard {
                    kind: "AdaptiveCard",
                    version: "1.4",
                    body: [TextBlock {
                        kind: "TextBlock",
                        text,
                        wrap: true,
                    }],
                },
            }],
        }
    }
}

#[async_trait]
impl Notifier for MsTeamsNotifier {
    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        let text = notice.plain_text();
        post_json(&self.client, &self.webhook_url, &Self::message(&text)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_matches_workflows_wire_shape() {
        let v = serde_json::to_value(MsTeamsNotifier::message("api-prod — major incident OPEN"))
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "message",
                "attachments": [{
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "content": {
                        "type": "AdaptiveCard",
                        "version": "1.4",
                        "body": [{
                            "type": "TextBlock",
                            "text": "api-prod — major incident OPEN",
                            "wrap": true
                        }]
                    }
                }]
            })
        );
    }
}
