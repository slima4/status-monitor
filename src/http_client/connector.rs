use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use metrics::Histogram;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tower::Service;

use crate::http_client::dns::HickoryDnsResolver;
use crate::http_client::pool_stats::{AliveGuard, PoolStats};
use crate::security::SsrfGuard;

pub(crate) struct ConnectorInner {
    pub(crate) resolver: Arc<HickoryDnsResolver>,
    pub(crate) tls: Arc<TlsConnector>,
    pub(crate) pool_stats: Arc<PoolStats>,
    pub(crate) ssrf_guard: SsrfGuard,
    pub(crate) connect_ms: Histogram,
    pub(crate) tls_ms: Histogram,
    pub(crate) connect_timeout: Duration,
    pub(crate) tcp_keepalive: Option<Duration>,
    pub(crate) tcp_nodelay: bool,
}

#[derive(Clone)]
pub struct PhaseConnector {
    pub(crate) inner: Arc<ConnectorInner>,
}

impl Service<Uri> for PhaseConnector {
    type Response = TokioIo<TrackedStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let inner = self.inner.clone();
        Box::pin(async move {
            let host = dst
                .host()
                .ok_or_else(|| io::Error::other("missing host"))?
                .to_owned();
            let scheme = dst.scheme_str().unwrap_or("http");
            let is_https = scheme == "https";
            let port = dst.port_u16().unwrap_or(if is_https { 443 } else { 80 });

            let stream = connect_tcp(&inner, &host, port).await?;

            let payload = if is_https {
                let tls_start = Instant::now();
                let server_name = ServerName::try_from(host)
                    .map_err(|e| io::Error::other(format!("invalid server name: {e}")))?;
                let tls_stream = inner.tls.connect(server_name, stream).await?;
                inner.tls_ms.record(tls_start.elapsed().as_millis() as f64);
                MaybeTls::Tls(Box::new(tls_stream))
            } else {
                MaybeTls::Plain(stream)
            };

            let alpn_h2 = match &payload {
                MaybeTls::Tls(s) => {
                    let (_io, conn): (&TcpStream, &rustls::ClientConnection) = s.get_ref();
                    conn.alpn_protocol() == Some(b"h2")
                }
                MaybeTls::Plain(_) => false,
            };

            let tracked = TrackedStream {
                inner: payload,
                _guard: AliveGuard::new(inner.pool_stats.clone()),
                alpn_h2,
            };
            Ok(TokioIo::new(tracked))
        })
    }
}

async fn connect_tcp(inner: &ConnectorInner, host: &str, port: u16) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = inner
        .resolver
        .resolve_addrs(host)
        .await
        .map_err(io::Error::other)?
        .into_iter()
        .filter(|ip| inner.ssrf_guard.allow(*ip))
        .map(|ip| SocketAddr::new(ip, port))
        .collect();

    if addrs.is_empty() {
        return Err(io::Error::other(format!("no allowed addresses for {host}")));
    }

    // Happy-Eyeballs v2 (RFC 8305): race v6 and v4 with a 250 ms stagger.
    // A broken AAAA on a long-tail user target otherwise costs the full
    // `connect_timeout` per check, missing subsequent intervals.
    // `happy_eyeballs::connect` bounds the overall sweep at the same budget
    // we passed previously to `tokio::time::timeout`.
    let tcp_start = Instant::now();
    let stream = crate::net::happy_eyeballs::connect(addrs, inner.connect_timeout).await?;

    inner
        .connect_ms
        .record(tcp_start.elapsed().as_millis() as f64);
    if inner.tcp_nodelay {
        let _ = stream.set_nodelay(true);
    }
    if let Some(d) = inner.tcp_keepalive {
        let sock = socket2::SockRef::from(&stream);
        let ka = socket2::TcpKeepalive::new().with_time(d).with_interval(d);
        let _ = sock.set_tcp_keepalive(&ka);
    }
    Ok(stream)
}

enum MaybeTls {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

pub struct TrackedStream {
    inner: MaybeTls,
    _guard: AliveGuard,
    alpn_h2: bool,
}

impl Connection for TrackedStream {
    fn connected(&self) -> Connected {
        let c = Connected::new();
        if self.alpn_h2 { c.negotiated_h2() } else { c }
    }
}

// Box<TlsStream<_>> and TcpStream are both Unpin, so Pin::new(&mut ...) works
// directly on either arm without needing a unifying trait object.
macro_rules! delegate_io {
    ($self:ident.$method:ident($($arg:expr),*)) => {
        match &mut $self.inner {
            MaybeTls::Plain(s) => Pin::new(s).$method($($arg),*),
            MaybeTls::Tls(s) => Pin::new(s).$method($($arg),*),
        }
    };
}

impl AsyncRead for TrackedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate_io!(self.poll_read(cx, buf))
    }
}

impl AsyncWrite for TrackedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate_io!(self.poll_write(cx, buf))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate_io!(self.poll_flush(cx))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate_io!(self.poll_shutdown(cx))
    }

    fn is_write_vectored(&self) -> bool {
        match &self.inner {
            MaybeTls::Plain(s) => s.is_write_vectored(),
            MaybeTls::Tls(s) => s.is_write_vectored(),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        delegate_io!(self.poll_write_vectored(cx, bufs))
    }
}
