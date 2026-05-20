use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use opentelemetry::trace::TraceContextExt;
use status_monitor::{
    api::build_router,
    app::AppState,
    config::AppConfig,
    error::{AppError, Result},
    http_client::client::build_clients,
    jobs::retention,
    notifier::engine::AlertEngine,
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
        MaintenanceStore, NotificationChannelStore, PgIncidentNarrationStore, PgMaintenanceStore,
        PgNotificationChannelStore, PostgresTargetStore, ResultSink, ResultsStore, TargetStore,
        ensure_default_org,
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
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing_opentelemetry::OpenTelemetrySpanExt;

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// One INFO line per HTTP request for ops / Loki — method, path,
/// status, latency, trace_id. Emitted at INFO and NOT coupled to the
/// `http.request` span's level: it fires whenever the global filter
/// admits INFO, regardless of whether that DEBUG span exists. Layered
/// *inside* the span solely so the request's OTLP `trace_id` is in
/// scope — emitting it on this line makes it the Loki→Tempo join key
/// (a Loki derived field on `trace_id` pivots straight to the trace).
///
/// Caveat: `trace_id` is present only when the `http.request` span is
/// actually recorded — i.e. `status_monitor` at DEBUG in the active
/// filter. Under a bare `info` `RUST_LOG` the span is absent and
/// `trace_id` is simply omitted (never a fake id) — the access line
/// still logs, but the Loki→Tempo pivot silently goes dark. The deploy
/// `RUST_LOG` variable must keep `status_monitor=debug`.
async fn access_log(req: Request, next: Next) -> Response {
    // Decide skip from the BORROWED path; only own method+path when
    // we will actually log. Caddy active-health + the deploy gate poll
    // /healthz//readyz forever — never allocate a String for them.
    let p = req.uri().path();
    let logged = (p != "/healthz" && p != "/readyz").then(|| {
        // Path only, never the query string: /auth/* carries
        // single-use magic-link tokens and the OAuth code/state,
        // which must not reach stdout logs.
        (req.method().clone(), req.uri().path().to_owned())
    });
    let start = Instant::now();
    let resp = next.run(req).await;
    if let Some((method, path)) = logged {
        // The enclosing tower_http span carries the exported OTLP
        // context — valid only when that span was recorded (see the
        // caveat above); otherwise the field is omitted, never faked.
        let ctx = tracing::Span::current().context();
        let span = ctx.span();
        let span_ctx = span.span_context();
        let trace_id = span_ctx.is_valid().then(|| span_ctx.trace_id().to_string());
        tracing::info!(
            method = %method,
            path = %path,
            status = resp.status().as_u16(),
            latency_ms = start.elapsed().as_millis() as u64,
            trace_id = trace_id.as_deref(),
            "http access"
        );
    }
    resp
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = AppConfig::load()?;
    // Inconsistent trace-export config (missing endpoint/credentials,
    // out-of-range sample ratio) is a clean startup error — validated
    // BEFORE tracing::init builds the exporter, so a bad value never
    // reaches the sampler/transport.
    cfg.validate_observability()?;
    let tracing_guard = observability::tracing::init(&cfg.observability);
    status_monitor::app::assert_per_org_status_config(&cfg);
    // A bad quota/rate/interval number is a clean startup config error,
    // never a `.expect()` crash-loop in router/layer construction (I6).
    cfg.validate_quotas_and_limits()?;
    // Same contract for the abuse rules: a malformed URL-pattern regex or
    // deny-list YAML fails fast here, not as a runtime panic.
    status_monitor::security::AbuseGuard::validate(&cfg.abuse)?;

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
        // SAFE: operator's own default-org config, not a data subject's slug
        org_id = %default_org_id,
        slug = %cfg.tenancy.default_org_slug,
        "default org ready"
    );
    let target_store: Arc<dyn TargetStore> = Arc::new(PostgresTargetStore::from_pool(
        pg_pool.clone(),
        cipher.clone(),
    ));

    tracing::info!(
        // SAFE: operator infra endpoint (no credentials — password is a
        // separate SecretString field), not user data
        url = %cfg.storage.clickhouse.url,
        database = %cfg.storage.clickhouse.database,
        "connecting to clickhouse"
    );
    let clickhouse_client = storage::build_client(&cfg.storage.clickhouse);
    storage::migrate(&clickhouse_client).await?;
    let result_sink: Arc<dyn ResultSink> =
        Arc::new(ClickhouseResultSink::from_client(clickhouse_client.clone()));
    let result_sink_for_state = result_sink.clone();
    let ch_client_for_public = clickhouse_client.clone();
    let ch_client_for_purge = clickhouse_client.clone();
    let results_store: Arc<dyn ResultsStore> =
        Arc::new(ClickhouseResultsStore::from_client(clickhouse_client));

    let http_clients = Arc::new(build_clients(
        &cfg.http_client,
        &cfg.checker,
        &cfg.dns,
        &cfg.security,
    )?);
    let http_pool_stats = http_clients.pool_stats().clone();

    let (result_tx, result_rx) = mpsc::channel(cfg.storage.clickhouse.buffer_size.max(1024));
    // Notification channels are per-org and edited at runtime, so the alert
    // path is always wired (no global enable gate). The engine resolves a
    // target's bound channels from this store per result.
    let notification_channel_store: Arc<dyn NotificationChannelStore> = Arc::new(
        PgNotificationChannelStore::new(pg_pool.clone(), cipher.clone()),
    );
    let (alert_tx, alert_rx) = mpsc::channel(cfg.storage.clickhouse.buffer_size.max(1024));
    let fanout = ResultFanout::new(result_tx.clone(), Some(alert_tx));
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
    let alert_engine_handle: Option<JoinHandle<()>> = Some({
        let token = root.clone();
        let engine = AlertEngine::new(
            alert_rx,
            notification_channel_store.clone(),
            status_monitor::http_outbound::build_outbound_client(),
        );
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
    let aggregator = Arc::new(OrgAggregator::new(
        pg_pool.clone(),
        ch_client_for_public,
        target_store.clone(),
        aggregator_cfg.clone(),
    ));
    let public_cache = PageCache::new(&cfg.public_status);
    let purge_cache = public_cache.clone();
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
        default_org_id,
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
        let grace_days = cfg.tenancy.deletion_grace_period_days;
        tokio::spawn(retention::run(
            pool,
            ch_client_for_purge,
            cfg.retention,
            cfg.auth.session.clone(),
            grace_days,
            purge_cache,
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
        notification_channel_store,
        incident_narration_store,
        default_org_id,
        outbound_http,
        email_sender,
    );
    // Hot-reload the abuse deny-lists on SIGHUP when enabled (validate then
    // atomic swap; a bad edit is rejected and the running rules stay).
    let abuse_reload_handle: Option<JoinHandle<()>> = status_monitor::security::abuse_reload::spawn(
        state.abuse.clone(),
        state.cfg.clone(),
        root.clone(),
    );

    // One span per HTTP request — the unit the OTLP layer exports; with
    // no instrumented span there is nothing to trace. DEBUG level so the
    // span is recorded only when the filter is at least debug.
    //
    // Layer order is load-bearing: with chained `Router::layer`, the
    // LAST `.layer` is OUTERMOST. TraceLayer added last → it wraps and
    // enters the `http.request` span before `access_log` runs, so
    // `access_log` can read the request's OTLP trace_id (see its doc).
    let router = build_router(state.clone(), root.clone())
        .merge(web::routes(state))
        .layer(middleware::from_fn(access_log))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::extract::Request| {
                    let path = req.uri().path();
                    // Caddy active-health and the deploy gate poll these
                    // on a tight loop forever; a span per probe at full
                    // sampling is pure noise with no diagnostic value.
                    if path == "/healthz" || path == "/readyz" {
                        return tracing::Span::none();
                    }
                    // Path only, never the query string: /auth/* carries
                    // single-use magic-link tokens and the OAuth
                    // code/state, which must not reach stdout logs or the
                    // exported span.
                    tracing::debug_span!(
                        "http.request",
                        method = %req.method(),
                        path = %path,
                    )
                })
                .on_response(DefaultOnResponse::new().level(tracing::Level::DEBUG)),
        );

    let listener = TcpListener::bind(&api_bind).await.map_err(AppError::Io)?;
    tracing::info!(
        // SAFE: operator API bind address, not a peer/user IP
        addr = %api_bind,
        "api listening"
    );

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
        if let Some(h) = abuse_reload_handle {
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
    // Flush and stop the OTLP batch exporter last, after the subscriber
    // has captured the final shutdown spans/logs.
    tracing_guard.shutdown();
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
