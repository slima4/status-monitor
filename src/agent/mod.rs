//! Stateless regional probe. Pulls its region's monitor config from the
//! control plane, runs the probe executors, ships results back. Runs no local
//! Postgres/ClickHouse/web/alerting — config lives in memory and a restart
//! re-pulls.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{self, AUTHORIZATION, IF_NONE_MATCH};
use hyper::{Request, StatusCode};
use secrecy::ExposeSecret;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::domain::{CheckResult, OrgId, Target};
use crate::error::{AppError, Result};
use crate::http_client::client::build_clients;
use crate::http_outbound::{self, OutboundHttpClient};
use crate::pipeline::{BatcherConfig, ResultBatcher};
use crate::scheduler::{Scheduler, TargetRegistry};
use crate::security::SsrfGuard;
use crate::storage::admin::EnabledTargetSource;
use crate::storage::{DomainExpiryStateStore, InMemoryDomainExpiryStateStore, ResultSink};
use crate::worker::domain_expiry::{DEFAULT_MAX_STALENESS, DomainExpiryRuntime};
use crate::worker::host_throttle::HostThrottle;
use crate::worker::rdap::RdapClient;
use crate::worker::rdap_singleflight::RdapSingleflight;
use crate::worker::{ResultFanout, WorkerPool};

const MAX_PULL_BYTES: usize = 32 << 20;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct AgentTargetDto {
    org_id: Uuid,
    target: Target,
}

#[derive(Deserialize)]
struct AgentTargetsResponse {
    #[allow(dead_code)]
    region: String,
    targets: Vec<AgentTargetDto>,
}

struct PullCache {
    etag: Option<String>,
    targets: Vec<(OrgId, Target)>,
}

/// [`EnabledTargetSource`] that pulls from the control plane instead of
/// Postgres. Caches the last response so a `304` or a transient control-plane
/// outage keeps the agent running on last-known config.
pub struct AgentPullSource {
    client: OutboundHttpClient,
    url: String,
    token: String,
    cache: Mutex<PullCache>,
}

impl AgentPullSource {
    pub fn new(client: OutboundHttpClient, base_url: &str, token: String) -> Self {
        Self {
            client,
            url: format!("{}/api/agent/targets", base_url.trim_end_matches('/')),
            token,
            cache: Mutex::new(PullCache {
                etag: None,
                targets: Vec::new(),
            }),
        }
    }

    async fn cached_or_err(&self, msg: String) -> Result<Vec<(OrgId, Target)>> {
        let cache = self.cache.lock().await;
        if cache.etag.is_some() || !cache.targets.is_empty() {
            tracing::warn!(error = %msg, "agent config pull failed; serving last-known config");
            Ok(cache.targets.clone())
        } else {
            Err(AppError::Other(anyhow::anyhow!(msg)))
        }
    }
}

#[async_trait]
impl EnabledTargetSource for AgentPullSource {
    async fn list_all_enabled_targets(&self) -> Result<Vec<(OrgId, Target)>> {
        let etag = self.cache.lock().await.etag.clone();
        let mut builder =
            Request::get(&self.url).header(AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(e) = &etag {
            builder = builder.header(IF_NONE_MATCH, e.clone());
        }
        let req = builder
            .body(Full::new(Bytes::new()))
            .context("building config-pull request")?;

        let resp = match tokio::time::timeout(HTTP_TIMEOUT, self.client.request(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return self
                    .cached_or_err(format!("config pull request failed: {e}"))
                    .await;
            }
            Err(_) => {
                return self
                    .cached_or_err("config pull request timed out".into())
                    .await;
            }
        };

        let status = resp.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(self.cache.lock().await.targets.clone());
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            // Terminal: the token was revoked or the agent disabled. Drop the
            // cached config and schedule nothing — serving last-known here would
            // keep probing (and holding decrypted credentials) after an operator
            // pulled the plug. Resumes automatically if the credential is restored.
            let mut cache = self.cache.lock().await;
            cache.etag = None;
            cache.targets.clear();
            tracing::error!(%status, "agent credential rejected; pausing until restored");
            return Ok(Vec::new());
        }
        if !status.is_success() {
            // Transient (5xx, connect, timeout): keep running on last-known config.
            return self
                .cached_or_err(format!("control plane returned {status} on config pull"))
                .await;
        }

        let new_etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = Limited::new(resp.into_body(), MAX_PULL_BYTES)
            .collect()
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("reading config-pull body: {e}")))?
            .to_bytes();
        let parsed: AgentTargetsResponse =
            serde_json::from_slice(&bytes).context("decoding config-pull response")?;
        let targets: Vec<(OrgId, Target)> = parsed
            .targets
            .into_iter()
            .map(|d| (OrgId(d.org_id), d.target))
            .collect();

        let mut cache = self.cache.lock().await;
        cache.etag = new_etag;
        cache.targets = targets.clone();
        Ok(targets)
    }
}

#[derive(serde::Serialize)]
struct IngestBody<'a> {
    batch_id: Uuid,
    results: &'a [CheckResult],
}

/// [`ResultSink`] that POSTs batches to the control-plane ingest API. Region
/// and agent id are not sent — the control plane derives them from the bearer
/// token, so they can't be spoofed. A fresh `batch_id` per call, reused across
/// the internal retries, makes a lost ack idempotent.
pub struct AgentResultSink {
    client: OutboundHttpClient,
    url: Url,
    token: String,
}

#[async_trait]
impl ResultSink for AgentResultSink {
    async fn write_batch(&self, results: &[CheckResult]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        // One id for this batch, reused across the internal retries below so a
        // lost ack (POST landed, response dropped) re-sends the SAME id and the
        // dashboard dedups it instead of double-writing. Retrying here — not
        // letting the batcher re-flush — is what keeps the id stable: the
        // batcher reforms (supersets) the buffer between ticks.
        let batch_id = Uuid::new_v4();
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            format!("Bearer {}", self.token),
        );
        let backoff = ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(200))
            .with_max_interval(Duration::from_secs(5))
            .with_max_elapsed_time(Some(Duration::from_secs(30)))
            .build();
        let op = || async {
            let body = IngestBody { batch_id, results };
            http_outbound::post_json_with_headers(&self.client, &self.url, &body, &headers)
                .await
                .map_err(backoff::Error::transient)
        };
        backoff::future::retry(backoff, op).await
    }
}

/// Run the process as a regional agent until SIGINT/SIGTERM.
pub async fn run(cfg: AppConfig) -> Result<()> {
    let region = cfg.agent.region.clone();
    let base = cfg
        .agent
        .control_plane_url
        .trim_end_matches('/')
        .to_string();
    let token = cfg.agent.token.expose_secret().to_string();
    tracing::info!(
        region = %region,
        control_plane = %base,
        "starting regional agent"
    );

    // Control plane is a public host → strict SSRF guard is correct.
    let outbound = http_outbound::build_outbound_client(SsrfGuard::strict());

    let source: Arc<dyn EnabledTargetSource> =
        Arc::new(AgentPullSource::new(outbound.clone(), &base, token.clone()));
    let results_url = Url::parse(&format!("{base}/api/agent/results"))
        .context("agent.control_plane_url is not a valid URL")?;
    let sink: Arc<dyn ResultSink> = Arc::new(AgentResultSink {
        client: outbound,
        url: results_url,
        token,
    });

    let http_clients = Arc::new(build_clients(
        &cfg.http_client,
        &cfg.checker,
        &cfg.dns,
        &cfg.security,
    )?);
    let (result_tx, result_rx) = tokio::sync::mpsc::channel(cfg.agent.buffer_capacity.max(1024));
    let fanout = ResultFanout::new(result_tx);
    let host_throttle = Arc::new(HostThrottle::new(
        cfg.checker.per_host_max_inflight,
        cfg.checker.rdap_max_inflight,
    ));
    let rdap_client = Arc::new(RdapClient::new(http_outbound::build_outbound_client(
        SsrfGuard::strict(),
    )));
    let domain_state: Arc<dyn DomainExpiryStateStore> =
        Arc::new(InMemoryDomainExpiryStateStore::new());
    let domain_runtime = Arc::new(DomainExpiryRuntime::new(
        rdap_client,
        Arc::new(RdapSingleflight::with_default_ttl()),
        domain_state,
        host_throttle.clone(),
        DEFAULT_MAX_STALENESS,
    ));
    let pool = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks,
        (*http_clients).clone(),
        cfg.circuit_breaker,
        fanout,
        host_throttle,
        domain_runtime,
    ));
    let registry = Arc::new(TargetRegistry::new(source));
    let mut sched_cfg = cfg.scheduler.clone();
    sched_cfg.target_refresh_interval_secs = cfg.agent.pull_interval_secs;
    let scheduler = Arc::new(Scheduler::new(registry, pool, sched_cfg));
    let batcher = ResultBatcher::new(
        result_rx,
        sink,
        BatcherConfig {
            batch_size: cfg.storage.clickhouse.batch_size.max(1),
            batch_timeout: Duration::from_secs(cfg.agent.flush_interval_secs.max(1)),
        },
    );

    let root = CancellationToken::new();
    let scheduler_handle = {
        let scheduler = scheduler.clone();
        let token = root.clone();
        tokio::spawn(async move {
            if let Err(err) = scheduler.run(token).await {
                tracing::error!(?err, "agent scheduler exited with error");
            }
        })
    };
    let batcher_handle = {
        let token = root.clone();
        tokio::spawn(async move { batcher.run(token).await })
    };

    wait_for_signal().await;
    tracing::info!("agent shutting down");
    root.cancel();
    let _ = scheduler_handle.await;
    let _ = batcher_handle.await;
    Ok(())
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
