use std::time::Duration;

use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use clickhouse::{Client, Row};
use serde::Serialize;
use uuid::Uuid;

use crate::config::ClickhouseConfig;
use crate::domain::CheckResult;
use crate::error::Result;
use crate::storage::traits::ResultSink;

const TABLE: &str = "check_results";

pub struct ClickhouseResultSink {
    client: Client,
}

impl ClickhouseResultSink {
    pub fn new(cfg: &ClickhouseConfig) -> Self {
        let mut client = Client::default()
            .with_url(&cfg.url)
            .with_database(&cfg.database)
            .with_user(&cfg.user);
        if !cfg.password.is_empty() {
            client = client.with_password(&cfg.password);
        }
        Self { client }
    }

    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    async fn write_once(
        &self,
        rows: &[CheckResultRow<'_>],
    ) -> std::result::Result<(), clickhouse::error::Error> {
        let mut insert = self.client.insert::<CheckResultRow>(TABLE).await?;
        for row in rows {
            insert.write(row).await?;
        }
        insert.end().await
    }
}

#[async_trait]
impl ResultSink for ClickhouseResultSink {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        let rows: Vec<CheckResultRow<'_>> = results.iter().map(CheckResultRow::from).collect();

        let backoff = ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(100))
            .with_max_interval(Duration::from_secs(5))
            .with_max_elapsed_time(Some(Duration::from_secs(30)))
            .build();

        // Full re-send on each retry is intentional: ClickHouse `insert_deduplicate` /
        // ReplicatedMergeTree dedup handle partial writes server-side. Don't checkpoint
        // mid-batch here — that races against the server's own dedup window.
        let op = || async {
            self.write_once(&rows)
                .await
                .map_err(backoff::Error::transient)
        };

        if let Err(err) = backoff::future::retry(backoff, op).await {
            tracing::error!(
                ?err,
                count = rows.len(),
                "clickhouse insert failed after retries"
            );
            return Err(anyhow::anyhow!("clickhouse insert failed: {err}").into());
        }
        Ok(())
    }
}

#[derive(Debug, Row, Serialize)]
struct CheckResultRow<'a> {
    target_id: Uuid,
    timestamp: i64,
    status: &'a str,
    duration_ms: u32,
    dns_ms: Option<u16>,
    connect_ms: Option<u16>,
    tls_ms: Option<u16>,
    ttfb_ms: Option<u16>,
    response_code: Option<u16>,
    response_size: Option<u32>,
    error: Option<&'a str>,
}

impl<'a> From<&'a CheckResult> for CheckResultRow<'a> {
    fn from(r: &'a CheckResult) -> Self {
        Self {
            target_id: r.target_id,
            timestamp: r.timestamp.timestamp_millis(),
            status: r.status.as_str(),
            duration_ms: r.duration_ms,
            dns_ms: r.dns_ms,
            connect_ms: r.connect_ms,
            tls_ms: r.tls_ms,
            ttfb_ms: r.ttfb_ms,
            response_code: r.response_code,
            response_size: r.response_size,
            error: r.error.as_deref(),
        }
    }
}
