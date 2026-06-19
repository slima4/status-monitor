//! Background fan-out of public incident updates to confirmed subscribers.
//! Independent of the escalation engine (feature-flag gated) so public incidents
//! notify regardless. The claim is an insert into the delivery log, so a crash
//! or a second replica never double-sends.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::email::{EmailAddress, EmailSender, EmailTemplate, TransactionalEmail};
use crate::storage::subscribers::{self, PendingUpdate};

const SEND_CONCURRENCY: usize = 8;

pub struct SubscriberDispatchConfig {
    pub tick_interval: Duration,
    pub batch_limit: i64,
    /// Wildcard base domain for `{slug}.{base_domain}` page links; empty on a
    /// self-host deploy, where `public_base_url` is used instead.
    pub base_domain: String,
    pub public_base_url: String,
    pub unsubscribe_secret: String,
    pub from_address: String,
    pub from_name: String,
}

pub struct SubscriberDispatcher {
    pool: PgPool,
    email: Arc<dyn EmailSender>,
    cfg: SubscriberDispatchConfig,
}

impl SubscriberDispatcher {
    pub fn new(pool: PgPool, email: Arc<dyn EmailSender>, cfg: SubscriberDispatchConfig) -> Self {
        Self { pool, email, cfg }
    }

    pub async fn run(&self, shutdown: CancellationToken) {
        let mut ticker = interval(self.cfg.tick_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("subscriber_dispatch: shutdown received");
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(err) = self.tick_once().await {
                        tracing::warn!(error = %err, "subscriber_dispatch tick failed");
                    }
                }
            }
        }
    }

    async fn tick_once(&self) -> anyhow::Result<()> {
        let pending = subscribers::list_pending_email(&self.pool, self.cfg.batch_limit).await?;
        let mut claimed = Vec::new();
        for p in pending {
            if subscribers::claim_notification(&self.pool, p.subscriber_id, p.update_id, p.org_id)
                .await?
            {
                claimed.push(p);
            }
        }
        // Sends are network-bound; run them concurrently so one slow recipient
        // can't stall the batch past the tick. Each marks its own outcome.
        stream::iter(claimed)
            .for_each_concurrent(SEND_CONCURRENCY, |p| async move {
                let error = self.deliver(&p).await.err().map(|e| e.to_string());
                if let Some(err) = &error {
                    tracing::warn!(error = %err, subscriber = %p.subscriber_id, "subscriber delivery failed");
                }
                if let Err(err) =
                    subscribers::mark_notification(&self.pool, p.subscriber_id, p.update_id, error.as_deref())
                        .await
                {
                    tracing::warn!(error = %err, "subscriber mark failed");
                }
            })
            .await;
        Ok(())
    }

    async fn deliver(&self, p: &PendingUpdate) -> anyhow::Result<()> {
        let outgoing = TransactionalEmail {
            from: EmailAddress::new(self.cfg.from_address.clone(), self.cfg.from_name.clone()),
            to: EmailAddress::new(p.target.clone(), p.target.clone()),
            template: EmailTemplate::SubscriberIncident {
                page_name: p.page_name.clone(),
                incident_title: p.incident_title.clone(),
                phase: p.phase.clone(),
                message: p.message.clone(),
                incident_url: self.incident_url(p),
                unsubscribe_url: self.unsubscribe_url(p),
            },
        };
        self.email
            .send(outgoing)
            .await
            .map_err(|e| anyhow::anyhow!("subscriber send: {e}"))?;
        Ok(())
    }

    fn page_origin(&self, p: &PendingUpdate) -> String {
        crate::web::host::page_origin(
            &self.cfg.base_domain,
            &self.cfg.public_base_url,
            &p.slug,
            p.custom_domain.as_deref(),
            p.custom_domain_verified,
        )
    }

    fn incident_url(&self, p: &PendingUpdate) -> String {
        format!("{}/status/incidents/{}", self.page_origin(p), p.incident_id)
    }

    fn unsubscribe_url(&self, p: &PendingUpdate) -> String {
        let mac = subscribers::unsubscribe_token(&self.cfg.unsubscribe_secret, p.subscriber_id);
        format!(
            "{}/subscribe/unsubscribe?s={}&t={mac}",
            self.page_origin(p),
            p.subscriber_id
        )
    }
}
