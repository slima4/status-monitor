use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use secrecy::ExposeSecret;
use tracing_subscriber::{EnvFilter, Layer, fmt, prelude::*};

use crate::config::{GrafanaConfig, LogFormat, ObservabilityConfig};

/// Held by `main` for the process lifetime. On shutdown it flushes and
/// stops the OTLP batch exporter so in-flight spans are not lost. A
/// no-op when trace export is disabled.
#[must_use]
pub struct TracingGuard {
    provider: Option<SdkTracerProvider>,
}

impl TracingGuard {
    pub fn shutdown(self) {
        if let Some(provider) = self.provider {
            if let Err(err) = provider.force_flush() {
                tracing::warn!(?err, "otlp span flush failed on shutdown");
            }
            if let Err(err) = provider.shutdown() {
                tracing::warn!(?err, "otlp tracer provider shutdown failed");
            }
        }
    }
}

pub fn init(cfg: &ObservabilityConfig) -> TracingGuard {
    // Lightpanda's CDP server emits messages chromiumoxide can't model, so its
    // conn/handler layers log an ERROR per message during a flow run even though
    // the flow itself succeeds. The flow verdict comes from the step runner, not
    // these logs, so silence the two noisy targets to keep the stream (and the
    // error-rate alerts) clean. Any RUST_LOG override still layers on top.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.log_level))
        .add_directive(
            "chromiumoxide::conn=off"
                .parse()
                .expect("static filter directive"),
        )
        .add_directive(
            "chromiumoxide::handler=off"
                .parse()
                .expect("static filter directive"),
        );

    let fmt_layer = match cfg.log_format {
        LogFormat::Json => fmt::layer().json().boxed(),
        LogFormat::Pretty => fmt::layer().pretty().boxed(),
    };

    // Export only when both switches are on. A build failure here must
    // never take down monitoring — log to stderr (the subscriber is not
    // installed yet) and continue without the OTLP layer. Empty/missing
    // credentials are already rejected at config validation, so a
    // failure at this point is a transport/runtime problem, not config.
    let (otel_layer, provider) = if cfg.tracing_enabled && cfg.grafana.enabled {
        match build_tracer_provider(&cfg.grafana) {
            Ok(provider) => {
                let tracer = provider.tracer("uptimepage");
                opentelemetry::global::set_text_map_propagator(
                    opentelemetry_sdk::propagation::TraceContextPropagator::new(),
                );
                (
                    Some(tracing_opentelemetry::layer().with_tracer(tracer)),
                    Some(provider),
                )
            }
            Err(err) => {
                eprintln!("otlp trace export disabled: {err:#}");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    // The subscriber is installed now, so this is the first point a
    // structured log can record the trace-export decision. Without it the
    // only signal is silence, which is indistinguishable from a disabled
    // exporter — mirror the "metrics listening" line so the boot log
    // states plainly whether spans are leaving the process.
    if provider.is_some() {
        tracing::info!(
            endpoint = %cfg.grafana.otlp_endpoint,
            sample_ratio = cfg.grafana.trace_sample_ratio,
            "otlp trace export enabled"
        );
    } else if cfg.tracing_enabled && cfg.grafana.enabled {
        // build_tracer_provider failed; the pre-subscriber eprintln
        // already carried the cause. Restate it through the subscriber so
        // it is not lost among the structured stdout.
        tracing::warn!("otlp trace export requested but exporter build failed");
    } else {
        tracing::debug!(
            "otlp trace export disabled (tracing_enabled and grafana.enabled not both set)"
        );
    }

    TracingGuard { provider }
}

fn build_tracer_provider(g: &GrafanaConfig) -> anyhow::Result<SdkTracerProvider> {
    // Basic base64(instance_id:api_key) — the Grafana Cloud OTLP gateway
    // auth. The token stays inside the SecretString until this point and
    // is never logged.
    let credentials = format!("{}:{}", g.instance_id, g.api_key.expose_secret());
    let authorization = format!("Basic {}", STANDARD.encode(credentials));

    // opentelemetry-otlp uses a programmatically-set HTTP endpoint
    // verbatim — it does NOT append the signal path (that only happens
    // for the OTEL_EXPORTER_OTLP_ENDPOINT env var). The operator config
    // is the OTLP base, so append `/v1/traces` here. Tolerate an
    // already-suffixed value so a full URL also works.
    let base = g.otlp_endpoint.trim_end_matches('/');
    let endpoint = if base.ends_with("/v1/traces") {
        base.to_string()
    } else {
        format!("{base}/v1/traces")
    };

    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_protocol(Protocol::HttpBinary)
        .with_headers(HashMap::from([(
            "authorization".to_string(),
            authorization,
        )]))
        .build()?;

    let resource = Resource::builder()
        .with_service_name("uptimepage")
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            env!("CARGO_PKG_VERSION"),
        ))
        .build();

    let sampler = Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(g.trace_sample_ratio)));

    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(sampler)
        .with_batch_exporter(exporter)
        .build())
}
