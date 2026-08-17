use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use metrics::Histogram;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::http_client::dns::HickoryDnsResolver;
use crate::security::SsrfGuard;

pub(crate) type ReqBody = Full<Bytes>;

/// Elapsed since `start` as whole ms, saturating at `u16::MAX` — the width the
/// result schema stores each phase in.
fn ms16(start: Instant) -> u16 {
    start.elapsed().as_millis().min(u16::MAX as u128) as u16
}

/// Everything a single fresh-connect probe needs. Borrowed from `HttpClients`
/// per check; the `tls` connector is the verify/insecure one already chosen.
pub(crate) struct ConnectParams<'a> {
    pub resolver: &'a HickoryDnsResolver,
    pub ssrf_guard: SsrfGuard,
    pub tls: &'a TlsConnector,
    pub connect_ms: &'a Histogram,
    pub tls_ms: &'a Histogram,
    pub connect_timeout: Duration,
    pub tcp_keepalive: Option<Duration>,
}

/// Per-phase wall times (ms) for one connection establishment. `tls_ms` is
/// `None` for plain HTTP. Feeds the breakdown chart's DNS/Connect/TLS bands;
/// saturates at `u16::MAX` (~65 s, far past any phase that didn't time out).
pub(crate) struct PhaseTimings {
    pub dns_ms: u16,
    pub connect_ms: u16,
    pub tls_ms: Option<u16>,
}

pub(crate) struct TimedConnection {
    pub stream: MaybeTls,
    pub timings: PhaseTimings,
    pub alpn_h2: bool,
}

/// Connect-phase failures, typed so the probe attributes each to the right
/// breakdown band / reason string instead of sniffing error text. Each variant
/// carries the phase times that *did* complete before the failure, so a TLS
/// failure can still report its DNS + TCP timings.
#[derive(Debug)]
pub(crate) enum ConnectError {
    Dns(anyhow::Error),
    NoAddrs {
        dns_ms: u16,
    },
    Connect {
        err: io::Error,
        dns_ms: u16,
    },
    Tls {
        err: io::Error,
        dns_ms: u16,
        connect_ms: u16,
    },
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Dns(e) => write!(f, "dns resolution failed: {e}"),
            ConnectError::NoAddrs { .. } => write!(f, "no allowed addresses for host"),
            ConnectError::Connect { err, .. } => write!(f, "tcp connect failed: {err}"),
            ConnectError::Tls { err, .. } => write!(f, "tls handshake failed: {err}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl ConnectError {
    /// Customer-facing reason naming *why* the connect phase failed, drilled
    /// out of the typed inner error rather than the flat phase name. DNS/TCP/TLS
    /// each surface their distinct failure modes (NXDOMAIN vs no-records,
    /// refused vs unreachable, expired vs untrusted vs hostname-mismatch).
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            ConnectError::Dns(e) => dns_reason(e),
            ConnectError::NoAddrs { .. } => "address not allowed",
            ConnectError::Connect { err, .. } => tcp_reason(err),
            ConnectError::Tls { err, .. } => tls_reason(err),
        }
    }

    /// `(dns_ms, connect_ms)` for the phases that completed before the failure.
    /// DNS failures have neither; a TLS failure has both.
    pub(crate) fn partial_timings(&self) -> (Option<u16>, Option<u16>) {
        match self {
            ConnectError::Dns(_) => (None, None),
            ConnectError::NoAddrs { dns_ms } | ConnectError::Connect { dns_ms, .. } => {
                (Some(*dns_ms), None)
            }
            ConnectError::Tls {
                dns_ms, connect_ms, ..
            } => (Some(*dns_ms), Some(*connect_ms)),
        }
    }
}

fn dns_reason(e: &anyhow::Error) -> &'static str {
    let Some(n) = e.downcast_ref::<hickory_resolver::net::NetError>() else {
        return "dns: lookup failed";
    };
    if n.is_nx_domain() {
        "dns: domain not found"
    } else if n.is_no_records_found() {
        "dns: no address records"
    } else if matches!(n, hickory_resolver::net::NetError::Timeout) {
        "dns: lookup timed out"
    } else {
        "dns: lookup failed"
    }
}

/// Shared with the TCP and TLS-cert kinds through `worker::connect_via_guard`.
/// Ping builds its own message and is not normalised through here.
pub(crate) fn tcp_reason(io: &io::Error) -> &'static str {
    match io.kind() {
        io::ErrorKind::ConnectionRefused => "connection refused",
        io::ErrorKind::HostUnreachable => "host unreachable",
        io::ErrorKind::NetworkUnreachable => "network unreachable",
        io::ErrorKind::ConnectionReset => "connection reset",
        io::ErrorKind::TimedOut => "connect timeout",
        _ => "connect",
    }
}

pub(crate) fn tls_reason(io: &io::Error) -> &'static str {
    use rustls::CertificateError;
    let Some(rustls::Error::InvalidCertificate(cert)) =
        io.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>())
    else {
        return "tls";
    };
    match cert {
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
            "certificate expired"
        }
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            "certificate not yet valid"
        }
        CertificateError::Revoked => "certificate revoked",
        CertificateError::UnknownIssuer => "certificate not trusted",
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            "certificate hostname mismatch"
        }
        _ => "certificate invalid",
    }
}

/// Resolve → TCP connect → (TLS handshake), timing each phase. No pooling: the
/// caller drives exactly one request over the returned stream, then drops it.
pub(crate) async fn timed_connect(
    p: &ConnectParams<'_>,
    host: &str,
    port: u16,
    is_https: bool,
) -> Result<TimedConnection, ConnectError> {
    let dns_start = Instant::now();
    let addrs: Vec<SocketAddr> = p
        .resolver
        .resolve_addrs(host)
        .await
        .map_err(ConnectError::Dns)?
        .into_iter()
        .filter(|ip| p.ssrf_guard.allow(*ip))
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    let dns_ms = ms16(dns_start);
    if addrs.is_empty() {
        return Err(ConnectError::NoAddrs { dns_ms });
    }

    // Happy-Eyeballs v2 (RFC 8305): race v6/v4 with a stagger, bounded by
    // `connect_timeout`, so a broken AAAA doesn't burn the whole budget.
    let connect_start = Instant::now();
    let stream = crate::net::happy_eyeballs::connect(addrs, p.connect_timeout)
        .await
        .map_err(|err| ConnectError::Connect { err, dns_ms })?;
    let connect_ms = ms16(connect_start);
    p.connect_ms.record(connect_ms as f64);
    let _ = stream.set_nodelay(true);
    if let Some(d) = p.tcp_keepalive {
        let sock = socket2::SockRef::from(&stream);
        let ka = socket2::TcpKeepalive::new().with_time(d).with_interval(d);
        let _ = sock.set_tcp_keepalive(&ka);
    }

    if !is_https {
        return Ok(TimedConnection {
            stream: MaybeTls::Plain(stream),
            timings: PhaseTimings {
                dns_ms,
                connect_ms,
                tls_ms: None,
            },
            alpn_h2: false,
        });
    }

    let server_name = ServerName::try_from(host.to_owned()).map_err(|e| ConnectError::Tls {
        err: io::Error::other(format!("invalid server name: {e}")),
        dns_ms,
        connect_ms,
    })?;
    let tls_start = Instant::now();
    let tls_stream = p
        .tls
        .connect(server_name, stream)
        .await
        .map_err(|err| ConnectError::Tls {
            err,
            dns_ms,
            connect_ms,
        })?;
    let tls_ms = ms16(tls_start);
    p.tls_ms.record(tls_ms as f64);
    let alpn_h2 = {
        let (_io, conn) = tls_stream.get_ref();
        conn.alpn_protocol() == Some(b"h2")
    };

    Ok(TimedConnection {
        stream: MaybeTls::Tls(Box::new(tls_stream)),
        timings: PhaseTimings {
            dns_ms,
            connect_ms,
            tls_ms: Some(tls_ms),
        },
        alpn_h2,
    })
}

/// Drive one HTTP request over an established stream. h1/h2 chosen by the
/// negotiated ALPN. The connection task is spawned and aborted by [`ConnGuard`]
/// once the response body is drained — fresh-connect means it serves one
/// request and is torn down.
pub(crate) async fn handshake(
    stream: MaybeTls,
    alpn_h2: bool,
) -> hyper::Result<(Sender, ConnGuard)> {
    let io = TokioIo::new(stream);
    if alpn_h2 {
        let (sender, conn) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), io).await?;
        let guard = ConnGuard(tokio::spawn(async move {
            let _ = conn.await;
        }));
        Ok((Sender::H2(sender), guard))
    } else {
        let (sender, conn) = hyper::client::conn::http1::handshake(io).await?;
        let guard = ConnGuard(tokio::spawn(async move {
            let _ = conn.await;
        }));
        Ok((Sender::H1(sender), guard))
    }
}

pub(crate) enum Sender {
    H1(hyper::client::conn::http1::SendRequest<ReqBody>),
    H2(hyper::client::conn::http2::SendRequest<ReqBody>),
}

impl Sender {
    pub(crate) async fn send_request(
        &mut self,
        req: Request<ReqBody>,
    ) -> hyper::Result<Response<Incoming>> {
        match self {
            Sender::H1(s) => s.send_request(req).await,
            Sender::H2(s) => s.send_request(req).await,
        }
    }
}

/// Aborts the spawned connection task on drop. Must outlive response-body
/// collection — the body streams from this task.
pub(crate) struct ConnGuard(tokio::task::JoinHandle<()>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// Box<TlsStream<_>> and TcpStream are both Unpin, so Pin::new(&mut ...) works
// directly on either arm.
pub(crate) enum MaybeTls {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

macro_rules! delegate_io {
    ($self:ident.$method:ident($($arg:expr),*)) => {
        match &mut $self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).$method($($arg),*),
            MaybeTls::Tls(s) => Pin::new(s).$method($($arg),*),
        }
    };
}

impl AsyncRead for MaybeTls {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate_io!(self.poll_read(cx, buf))
    }
}

impl AsyncWrite for MaybeTls {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate_io!(self.poll_write(cx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate_io!(self.poll_flush(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate_io!(self.poll_shutdown(cx))
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            MaybeTls::Plain(s) => s.is_write_vectored(),
            MaybeTls::Tls(s) => s.is_write_vectored(),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        delegate_io!(self.poll_write_vectored(cx, bufs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(kind: io::ErrorKind) -> ConnectError {
        ConnectError::Connect {
            err: io::Error::new(kind, "x"),
            dns_ms: 0,
        }
    }

    fn tls_cert(cert: rustls::CertificateError) -> ConnectError {
        ConnectError::Tls {
            err: io::Error::other(rustls::Error::InvalidCertificate(cert)),
            dns_ms: 0,
            connect_ms: 0,
        }
    }

    #[test]
    fn tcp_reasons_split_by_io_kind() {
        assert_eq!(
            tcp(io::ErrorKind::ConnectionRefused).reason(),
            "connection refused"
        );
        assert_eq!(
            tcp(io::ErrorKind::HostUnreachable).reason(),
            "host unreachable"
        );
        assert_eq!(
            tcp(io::ErrorKind::NetworkUnreachable).reason(),
            "network unreachable"
        );
        assert_eq!(
            tcp(io::ErrorKind::ConnectionReset).reason(),
            "connection reset"
        );
        assert_eq!(tcp(io::ErrorKind::TimedOut).reason(), "connect timeout");
        assert_eq!(tcp(io::ErrorKind::BrokenPipe).reason(), "connect");
    }

    #[test]
    fn tls_reasons_split_by_certificate_error() {
        use rustls::CertificateError as C;
        assert_eq!(tls_cert(C::Expired).reason(), "certificate expired");
        assert_eq!(
            tls_cert(C::NotValidYet).reason(),
            "certificate not yet valid"
        );
        assert_eq!(tls_cert(C::Revoked).reason(), "certificate revoked");
        assert_eq!(
            tls_cert(C::UnknownIssuer).reason(),
            "certificate not trusted"
        );
        assert_eq!(
            tls_cert(C::NotValidForName).reason(),
            "certificate hostname mismatch"
        );
        assert_eq!(tls_cert(C::BadEncoding).reason(), "certificate invalid");
    }

    #[test]
    fn tls_non_certificate_error_stays_generic() {
        let e = ConnectError::Tls {
            err: io::Error::other("handshake eof"),
            dns_ms: 0,
            connect_ms: 0,
        };
        assert_eq!(e.reason(), "tls");
    }

    #[test]
    fn no_addrs_is_address_not_allowed() {
        assert_eq!(
            ConnectError::NoAddrs { dns_ms: 0 }.reason(),
            "address not allowed"
        );
    }

    #[test]
    fn partial_timings_carry_completed_phases() {
        // DNS failure: nothing completed.
        assert_eq!(
            ConnectError::Dns(anyhow::anyhow!("x")).partial_timings(),
            (None, None)
        );
        // TCP failure: DNS completed, connect did not.
        let tcp = ConnectError::Connect {
            err: io::Error::other("x"),
            dns_ms: 12,
        };
        assert_eq!(tcp.partial_timings(), (Some(12), None));
        // TLS failure: DNS + TCP completed.
        let tls = ConnectError::Tls {
            err: io::Error::other(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Expired,
            )),
            dns_ms: 12,
            connect_ms: 45,
        };
        assert_eq!(tls.partial_timings(), (Some(12), Some(45)));
    }

    #[test]
    fn dns_timeout_and_unknown_split() {
        let t = ConnectError::Dns(anyhow::Error::new(hickory_resolver::net::NetError::Timeout));
        assert_eq!(t.reason(), "dns: lookup timed out");
        let other = ConnectError::Dns(anyhow::anyhow!("plain message"));
        assert_eq!(other.reason(), "dns: lookup failed");
    }
}
