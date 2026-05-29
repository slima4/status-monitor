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
/// breakdown band / reason string instead of sniffing error text.
#[derive(Debug)]
pub(crate) enum ConnectError {
    Dns(anyhow::Error),
    NoAddrs,
    Connect(io::Error),
    Tls(io::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Dns(e) => write!(f, "dns resolution failed: {e}"),
            ConnectError::NoAddrs => write!(f, "no allowed addresses for host"),
            ConnectError::Connect(e) => write!(f, "tcp connect failed: {e}"),
            ConnectError::Tls(e) => write!(f, "tls handshake failed: {e}"),
        }
    }
}

impl std::error::Error for ConnectError {}

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
        return Err(ConnectError::NoAddrs);
    }

    // Happy-Eyeballs v2 (RFC 8305): race v6/v4 with a stagger, bounded by
    // `connect_timeout`, so a broken AAAA doesn't burn the whole budget.
    let connect_start = Instant::now();
    let stream = crate::net::happy_eyeballs::connect(addrs, p.connect_timeout)
        .await
        .map_err(ConnectError::Connect)?;
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

    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|e| ConnectError::Tls(io::Error::other(format!("invalid server name: {e}"))))?;
    let tls_start = Instant::now();
    let tls_stream = p
        .tls
        .connect(server_name, stream)
        .await
        .map_err(ConnectError::Tls)?;
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
