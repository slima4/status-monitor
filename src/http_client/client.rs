use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper_rustls::ConfigBuilderExt;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use metrics::{Histogram, histogram};
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;

use crate::config::{CheckerConfig, DnsConfig, HttpClientConfig, SecurityConfig};
use crate::error::Result;
use crate::http_client::connector::{ConnectorInner, PhaseConnector};
use crate::http_client::dns::HickoryDnsResolver;
use crate::http_client::pool_stats::PoolStats;
use crate::observability::metrics::names;
use crate::security::SsrfGuard;

pub type ReqBody = Full<Bytes>;
pub type HyperHttpClient = HyperClient<PhaseConnector, ReqBody>;

#[derive(Clone)]
pub struct HttpClients {
    verifying: Arc<HyperHttpClient>,
    insecure: Arc<HyperHttpClient>,
    pub(crate) pool_stats: Arc<PoolStats>,
    pub(crate) ttfb_ms: Histogram,
    pub(crate) user_agent: Arc<str>,
    pub(crate) resolver: Arc<HickoryDnsResolver>,
    pub(crate) ssrf_guard: SsrfGuard,
}

impl HttpClients {
    pub fn pick(&self, verify_tls: bool) -> &HyperHttpClient {
        if verify_tls {
            self.verifying.as_ref()
        } else {
            self.insecure.as_ref()
        }
    }

    pub fn pool_stats(&self) -> &Arc<PoolStats> {
        &self.pool_stats
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub fn resolver(&self) -> &Arc<HickoryDnsResolver> {
        &self.resolver
    }

    pub fn ssrf_guard(&self) -> SsrfGuard {
        self.ssrf_guard
    }
}

pub fn build_clients(
    http_cfg: &HttpClientConfig,
    checker_cfg: &CheckerConfig,
    dns_cfg: &DnsConfig,
    security_cfg: &SecurityConfig,
) -> Result<HttpClients> {
    install_default_crypto_provider();

    let resolver = Arc::new(HickoryDnsResolver::new(dns_cfg)?);
    let pool_stats = PoolStats::new();
    let ssrf_guard = SsrfGuard::new(security_cfg.allow_private_targets);
    let connect_ms = histogram!(names::CHECK_CONNECT_MS);
    let tls_ms = histogram!(names::CHECK_TLS_MS);

    let shared = SharedConnect {
        resolver: resolver.clone(),
        pool_stats: pool_stats.clone(),
        ssrf_guard,
        connect_ms,
        tls_ms,
    };
    let verifying = build_one(http_cfg, checker_cfg, &shared, true)?;
    let insecure = build_one(http_cfg, checker_cfg, &shared, false)?;

    Ok(HttpClients {
        verifying: Arc::new(verifying),
        insecure: Arc::new(insecure),
        pool_stats,
        ttfb_ms: histogram!(names::CHECK_TTFB_MS),
        user_agent: Arc::from(http_cfg.user_agent.as_str()),
        resolver,
        ssrf_guard,
    })
}

struct SharedConnect {
    resolver: Arc<HickoryDnsResolver>,
    pool_stats: Arc<PoolStats>,
    ssrf_guard: SsrfGuard,
    connect_ms: Histogram,
    tls_ms: Histogram,
}

fn build_one(
    http_cfg: &HttpClientConfig,
    checker_cfg: &CheckerConfig,
    shared: &SharedConnect,
    verify_tls: bool,
) -> Result<HyperHttpClient> {
    let tls_config = build_tls_config(verify_tls, http_cfg.http2_prior_knowledge)?;
    let tls_connector = Arc::new(TlsConnector::from(Arc::new(tls_config)));

    let inner = Arc::new(ConnectorInner {
        resolver: shared.resolver.clone(),
        tls: tls_connector,
        pool_stats: shared.pool_stats.clone(),
        ssrf_guard: shared.ssrf_guard,
        connect_ms: shared.connect_ms.clone(),
        tls_ms: shared.tls_ms.clone(),
        connect_timeout: Duration::from_millis(checker_cfg.connect_timeout_ms),
        tcp_keepalive: Some(Duration::from_secs(http_cfg.tcp_keepalive_secs))
            .filter(|d| !d.is_zero()),
        tcp_nodelay: true,
    });
    let connector = PhaseConnector { inner };

    let mut builder = HyperClient::builder(TokioExecutor::new());
    builder
        .timer(TokioTimer::new())
        .pool_timer(TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(http_cfg.pool_idle_timeout_secs))
        .pool_max_idle_per_host(http_cfg.pool_max_idle_per_host)
        .http2_keep_alive_interval(Duration::from_secs(http_cfg.http2_keep_alive_interval_secs))
        .http2_keep_alive_timeout(Duration::from_secs(http_cfg.http2_keep_alive_timeout_secs))
        .http2_keep_alive_while_idle(http_cfg.http2_keep_alive_while_idle)
        .http2_adaptive_window(true);
    if http_cfg.http2_prior_knowledge {
        builder.http2_only(true);
    }

    Ok(builder.build(connector))
}

fn build_tls_config(verify: bool, h2_prior_knowledge: bool) -> Result<ClientConfig> {
    let builder = ClientConfig::builder();
    let mut cfg = if verify {
        match builder.with_native_roots() {
            Ok(b) => b.with_no_client_auth(),
            Err(_) => ClientConfig::builder()
                .with_webpki_roots()
                .with_no_client_auth(),
        }
    } else {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth()
    };

    cfg.alpn_protocols = if h2_prior_knowledge {
        vec![b"h2".to_vec()]
    } else {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    };
    Ok(cfg)
}

pub(crate) fn install_default_crypto_provider() {
    if CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

/// Accepts every chain. Used by the `verify_tls = false` HTTP client path and
/// by the TLS-cert-expiry check (which must read expired/self-signed leaves to
/// report `Down: expired` rather than a generic handshake failure).
#[derive(Debug)]
pub(crate) struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}
