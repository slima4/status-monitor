use std::collections::BTreeMap;

use async_trait::async_trait;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, post_json_with_headers};
use crate::notifier::Notifier;
use crate::notifier::event::{AlertEvent, IncidentNotice};

pub struct WebhookNotifier {
    client: OutboundHttpClient,
    url: Url,
    headers: BTreeMap<String, String>,
}

impl WebhookNotifier {
    pub fn new(client: OutboundHttpClient, url: Url, headers: BTreeMap<String, String>) -> Self {
        Self {
            client,
            url,
            headers,
        }
    }
}

#[async_trait]
impl Notifier for WebhookNotifier {
    async fn notify(&self, event: &AlertEvent) -> Result<()> {
        post_json_with_headers(&self.client, &self.url, event, &self.headers).await
    }

    async fn notify_incident(&self, notice: &IncidentNotice) -> Result<()> {
        post_json_with_headers(&self.client, &self.url, notice, &self.headers).await
    }
}
