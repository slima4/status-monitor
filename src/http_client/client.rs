use std::sync::Arc;
use std::time::Duration;

use hyper_rustls::ConfigBuilderExt;
use metrics::{Histogram, histogram};
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;

use crate::config::{CheckerConfig, DnsConfig, HttpClientConfig, SecurityConfig};
use crate::error::Result;
use crate::http_client::connector::ConnectParams;
use crate::http_client::dns::HickoryDnsResolver;
use crate::observability::metrics::names;
use crate::security::SsrfGuard;

/// Shared, cheaply-clonable handles for the check path. Holds no connection
/// pool: every HTTP check connects fresh (a monitor probes each target once per
/// interval, so pooling rarely reused a socket — and fresh-connect is what lets
/// the probe time DNS/connect/TLS per check). The two TLS connectors differ
/// only in cert verification.
#[derive(Clone)]
pub struct HttpClients {
    tls_verifying: Arc<TlsConnector>,
    tls_insecure: Arc<TlsConnector>,
    pub(crate) ttfb_ms: Histogram,
    connect_ms: Histogram,
    tls_ms: Histogram,
    pub(crate) user_agent: Arc<str>,
    pub(crate) resolver: Arc<HickoryDnsResolver>,
    pub(crate) ssrf_guard: SsrfGuard,
    connect_timeout: Duration,
    tcp_keepalive: Option<Duration>,
}

impl HttpClients {
    pub(crate) fn connect_params(&self, verify_tls: bool) -> ConnectParams<'_> {
        ConnectParams {
            resolver: &self.resolver,
            ssrf_guard: self.ssrf_guard,
            tls: if verify_tls {
                &self.tls_verifying
            } else {
                &self.tls_insecure
            },
            connect_ms: &self.connect_ms,
            tls_ms: &self.tls_ms,
            connect_timeout: self.connect_timeout,
            tcp_keepalive: self.tcp_keepalive,
        }
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
    let ssrf_guard = SsrfGuard::new(security_cfg.allow_private_targets);
    let tls_verifying = Arc::new(TlsConnector::from(Arc::new(build_tls_config(true)?)));
    let tls_insecure = Arc::new(TlsConnector::from(Arc::new(build_tls_config(false)?)));

    Ok(HttpClients {
        tls_verifying,
        tls_insecure,
        ttfb_ms: histogram!(names::CHECK_TTFB_MS),
        connect_ms: histogram!(names::CHECK_CONNECT_MS),
        tls_ms: histogram!(names::CHECK_TLS_MS),
        user_agent: Arc::from(http_cfg.user_agent.as_str()),
        resolver,
        ssrf_guard,
        connect_timeout: Duration::from_millis(checker_cfg.connect_timeout_ms),
        tcp_keepalive: Some(Duration::from_secs(http_cfg.tcp_keepalive_secs))
            .filter(|d| !d.is_zero()),
    })
}

fn build_tls_config(verify: bool) -> Result<ClientConfig> {
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

    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
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
