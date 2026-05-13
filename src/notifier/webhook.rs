use async_trait::async_trait;
use url::Url;

use crate::domain::AlertChannel;
use crate::error::Result;
use crate::notifier::Notifier;
use crate::notifier::event::AlertEvent;
use crate::notifier::transport::{NotifyHttpClient, post_json};

pub struct WebhookNotifier {
    client: NotifyHttpClient,
    url: Url,
}

impl WebhookNotifier {
    pub fn new(client: NotifyHttpClient, url: Url) -> Self {
        Self { client, url }
    }
}

#[async_trait]
impl Notifier for WebhookNotifier {
    fn channel(&self) -> AlertChannel {
        AlertChannel::Webhook
    }

    async fn notify(&self, event: &AlertEvent) -> Result<()> {
        post_json(&self.client, &self.url, event).await
    }
}
