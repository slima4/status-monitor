use std::sync::Arc;
use std::time::Duration;

use std::time::Duration as StdDuration;

use status_monitor::{
    api::build_router,
    app::AppState,
    config::AppConfig,
    error::{AppError, Result},
    http_client::client::build_clients,
    jobs::purge_deleted_orgs,
    notifier::{Notifier, build_notifiers, engine::AlertEngine},
    observability,
    pipeline::{BatcherConfig, ResultBatcher},
    public_status::{
        AggregatorConfig, IncidentWriter, IncidentWriterConfig, OrgAggregator, OrgPublicSource,
        PageCache, PgIncidentStore, PublicSource,
    },
    scheduler::{Scheduler, TargetRegistry},
    security::Cipher,
    storage::{
        self, ClickhouseResultSink, ClickhouseResultsStore, IncidentNarrationStore,
        MaintenanceStore, PgIncidentNarrationStore, PgMaintenanceStore, PostgresTargetStore,
        ResultSink, ResultsStore, TargetStore, ensure_default_org,
    },
    web,
    worker::{ResultFanout, WorkerPool},
};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = AppConfig::load()?;
    observability::tracing::init(&cfg.observability);
    status_monitor::app::assert_per_org_status_config(&cfg);

    let metrics_handle = if cfg.observability.metrics_enabled {
        Some(observability::metrics::init(&cfg.server.metrics_bind)?)
    } else {
        None
    };

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        api_bind = %cfg.server.api_bind,
        metrics_bind = %cfg.server.metrics_bind,
        "starting status-monitor"
    );

    let api_bind = cfg.server.api_bind.clone();
    let cipher = match cfg.security.kek() {
        Some(kek) => Some(Arc::new(Cipher::from_base64(kek).map_err(|e| {
            AppError::Other(anyhow::anyhow!("invalid credentials_kek_base64: {e}"))
        })?)),
        None => {
            tracing::warn!(
                "credentials_kek_base64 unset — basic_auth/bearer_token will be stored in plaintext"
            );
            None
        }
    };
    // Open the pool + run migrations first so `ensure_default_org` can write,
    // then construct the store stamped with the resolved org id.
    let pg_pool = PostgresTargetStore::connect_pool(&cfg.storage.postgres).await?;
    let default_org_id = ensure_default_org(&pg_pool, &cfg.tenancy.default_org_slug).await?;
    tracing::info!(
        org_id = %default_org_id,
        slug = %cfg.tenancy.default_org_slug,
        "default org ready"
    );
    let target_store: Arc<dyn TargetStore> = Arc::new(PostgresTargetStore::from_pool(
        pg_pool.clone(),
        cipher.clone(),
        default_org_id,
    ));

    tracing::info!(
        url = %cfg.storage.clickhouse.url,
        database = %cfg.storage.clickhouse.database,
        "connecting to clickhouse"
    );
    let clickhouse_client = storage::build_client(&cfg.storage.clickhouse);
    storage::migrate(&clickhouse_client).await?;
    let result_sink: Arc<dyn ResultSink> = Arc::new(ClickhouseResultSink::from_client(
        clickhouse_client.clone(),
        default_org_id,
    ));
    let result_sink_for_state = result_sink.clone();
    let ch_client_for_public = clickhouse_client.clone();
    let ch_client_for_purge = clickhouse_client.clone();
    let results_store: Arc<dyn ResultsStore> = Arc::new(ClickhouseResultsStore::from_client(
        clickhouse_client,
        default_org_id,
    ));

    let http_clients = Arc::new(build_clients(
        &cfg.http_client,
        &cfg.checker,
        &cfg.dns,
        &cfg.security,
    )?);
    let http_pool_stats = http_clients.pool_stats().clone();

    let (result_tx, result_rx) = mpsc::channel(cfg.storage.clickhouse.buffer_size.max(1024));
    let notifiers: Vec<Arc<dyn Notifier>> = build_notifiers(&cfg.notifications)?;
    let (alert_tx, alert_rx) = if notifiers.is_empty() {
        (None, None)
    } else {
        let (tx, rx) = mpsc::channel(cfg.storage.clickhouse.buffer_size.max(1024));
        (Some(tx), Some(rx))
    };
    let fanout = ResultFanout::new(result_tx.clone(), alert_tx);
    let pool = Arc::new(WorkerPool::new(
        cfg.checker.max_concurrent_checks,
        (*http_clients).clone(),
        cfg.circuit_breaker,
        fanout,
    ));
    let scheduler_source: Arc<dyn storage::admin::EnabledTargetSource> = Arc::new(
        storage::admin::AdminRepo::new(pg_pool.clone(), cipher.clone(), "scheduler_refresh"),
    );
    let registry = Arc::new(TargetRegistry::new(scheduler_source));
    let scheduler = Arc::new(Scheduler::new(
        registry.clone(),
        pool.clone(),
        cfg.scheduler.clone(),
    ));

    let batcher_cfg = BatcherConfig {
        batch_size: cfg.storage.clickhouse.batch_size.max(1),
        batch_timeout: Duration::from_millis(cfg.storage.clickhouse.batch_timeout_ms),
    };
    let batcher = ResultBatcher::new(result_rx, result_sink, batcher_cfg);

    let root = CancellationToken::new();
    let scheduler_handle: JoinHandle<()> = {
        let scheduler = scheduler.clone();
        let token = root.clone();
        tokio::spawn(async move {
            if let Err(err) = scheduler.run(token).await {
                tracing::error!(?err, "scheduler exited with error");
            }
        })
    };
    let batcher_handle: JoinHandle<()> = {
        let token = root.clone();
        tokio::spawn(async move { batcher.run(token).await })
    };
    let alert_engine_handle: Option<JoinHandle<()>> = alert_rx.map(|rx| {
        let token = root.clone();
        let engine = AlertEngine::new(rx, notifiers);
        tokio::spawn(async move { engine.run(token).await })
    });
    // Floor at 100ms to keep a misconfigured 0 / sub-tick value from spinning.
    let sample_interval =
        Duration::from_millis(cfg.observability.gauge_sample_interval_ms.max(100));
    let sampler_handle: JoinHandle<()> = status_monitor::observability::sampler::spawn(
        pool.clone(),
        registry.clone(),
        http_pool_stats,
        &result_tx,
        sample_interval,
        root.clone(),
    );
    drop(result_tx);

    let aggregator_cfg = AggregatorConfig::default();
    let cache_ttl = StdDuration::from_secs(10);
    let aggregator = Arc::new(OrgAggregator::new(
        pg_pool.clone(),
        ch_client_for_public,
        target_store.clone(),
        aggregator_cfg.clone(),
    ));
    let public_cache = PageCache::new(cache_ttl);
    let public_source: Arc<dyn PublicSource> = Arc::new(OrgPublicSource::new(
        aggregator,
        public_cache,
        pg_pool.clone(),
        aggregator_cfg.site_name.clone(),
    ));

    let pg_pool_for_stores = pg_pool.clone();
    let incident_writer = Arc::new(IncidentWriter::new(
        target_store.clone(),
        results_store.clone(),
        Arc::new(PgIncidentStore::new(pg_pool, default_org_id)),
        IncidentWriterConfig::default(),
    ));
    let incident_writer_handle: JoinHandle<()> = {
        let writer = incident_writer.clone();
        let token = root.clone();
        tokio::spawn(async move { writer.run(token).await })
    };

    let purge_handle: JoinHandle<()> = {
        let pool = pg_pool_for_stores.clone();
        let token = root.clone();
        let interval = Duration::from_secs(cfg.tenancy.purge_interval_secs.max(1));
        let grace_days = cfg.tenancy.deletion_grace_period_days;
        tokio::spawn(purge_deleted_orgs::run(
            pool,
            ch_client_for_purge,
            interval,
            grace_days,
            token,
        ))
    };

    let maintenance_store: Arc<dyn MaintenanceStore> = Arc::new(PgMaintenanceStore::new(
        pg_pool_for_stores.clone(),
        default_org_id,
    ));
    let incident_narration_store: Arc<dyn IncidentNarrationStore> = Arc::new(
        PgIncidentNarrationStore::new(pg_pool_for_stores.clone(), default_org_id),
    );

    let outbound_http = status_monitor::http_outbound::build_outbound_client();
    let email_sender = status_monitor::email::build_email_sender(&cfg.email, &outbound_http)
        .map_err(|e| AppError::Other(anyhow::anyhow!("build_email_sender: {e}")))?;

    if cfg.tenancy.enabled {
        status_monitor::auth::ensure_fingerprint_salt(
            &pg_pool_for_stores,
            &cfg.auth.fingerprint_salt,
        )
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("auth salt guard: {e}")))?;

        let prefix_len = cfg.auth.api_tokens.prefix_visible_chars as usize;
        if prefix_len < status_monitor::auth::api_tokens::MIN_PREFIX_VISIBLE_CHARS {
            return Err(AppError::Other(anyhow::anyhow!(
                "auth.api_tokens.prefix_visible_chars must be >= {} (got {prefix_len})",
                status_monitor::auth::api_tokens::MIN_PREFIX_VISIBLE_CHARS
            )));
        }
    }

    let invitation_purge_handle: JoinHandle<()> = {
        let pool = pg_pool_for_stores.clone();
        let token = root.clone();
        let keep_days = i64::from(cfg.auth.invitations.expiry_hours / 24).max(1);
        tokio::spawn(status_monitor::auth::invitations_cleanup::run(
            pool, keep_days, token,
        ))
    };

    let oauth_state_cleanup_handle: JoinHandle<()> =
        status_monitor::auth::oauth_state_cleanup::spawn(pg_pool_for_stores.clone(), root.clone());

    // Magic-link sweep only runs when the method is wired into the router.
    // When disabled the routes 404, no rows are ever inserted, and the ticker
    // would be dead weight.
    let magic_link_cleanup_handle: Option<JoinHandle<()>> = cfg
        .auth
        .enabled_methods
        .iter()
        .any(|m| m == "magic_link")
        .then(|| {
            let pool = pg_pool_for_stores.clone();
            let token = root.clone();
            tokio::spawn(status_monitor::auth::magic_link_cleanup::run(pool, token))
        });

    let state = AppState::new(
        cfg,
        Some(pg_pool_for_stores),
        target_store,
        results_store,
        result_sink_for_state,
        http_clients.clone(),
        pool.clone(),
        public_source,
        maintenance_store,
        incident_narration_store,
        default_org_id,
        outbound_http,
        email_sender,
    );
    let router =
        build_router(state.clone(), root.clone()).merge(web::routes(&state.cfg).with_state(state));

    let listener = TcpListener::bind(&api_bind).await.map_err(AppError::Io)?;
    tracing::info!(addr = %api_bind, "api listening");

    let signal_token = root.clone();
    let serve = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        wait_for_signal().await;
        signal_token.cancel();
    });

    if let Err(err) = serve.await {
        tracing::error!(?err, "api server error");
        root.cancel();
    }

    tracing::info!("draining background tasks");
    let drain = async {
        let _ = tokio::join!(
            scheduler_handle,
            batcher_handle,
            sampler_handle,
            incident_writer_handle,
            purge_handle,
            invitation_purge_handle,
            oauth_state_cleanup_handle,
        );
        if let Some(h) = alert_engine_handle {
            let _ = h.await;
        }
        if let Some(h) = magic_link_cleanup_handle {
            let _ = h.await;
        }
    };
    if timeout(SHUTDOWN_DEADLINE, drain).await.is_err() {
        tracing::warn!(
            deadline_secs = SHUTDOWN_DEADLINE.as_secs(),
            "shutdown deadline exceeded, dropping in-flight work"
        );
    }

    let _ = metrics_handle;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn wait_for_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}
