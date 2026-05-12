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

#[derive(Clone)]
pub struct PhaseConnector {
    pub resolver: Arc<HickoryDnsResolver>,
    pub tls: Arc<TlsConnector>,
    pub pool_stats: Arc<PoolStats>,
    pub connect_ms: Histogram,
    pub tls_ms: Histogram,
    pub connect_timeout: Duration,
    pub tcp_keepalive: Option<Duration>,
    pub tcp_nodelay: bool,
}

impl Service<Uri> for PhaseConnector {
    type Response = TokioIo<TrackedStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let resolver = self.resolver.clone();
        let tls = self.tls.clone();
        let pool_stats = self.pool_stats.clone();
        let connect_ms = self.connect_ms.clone();
        let tls_ms = self.tls_ms.clone();
        let connect_timeout = self.connect_timeout;
        let tcp_keepalive = self.tcp_keepalive;
        let tcp_nodelay = self.tcp_nodelay;

        Box::pin(async move {
            let host = dst
                .host()
                .ok_or_else(|| io::Error::other("missing host"))?
                .to_owned();
            let scheme = dst.scheme_str().unwrap_or("http");
            let is_https = scheme == "https";
            let port = dst.port_u16().unwrap_or(if is_https { 443 } else { 80 });

            let stream = connect_tcp(
                &resolver,
                &host,
                port,
                connect_timeout,
                tcp_keepalive,
                tcp_nodelay,
                &connect_ms,
            )
            .await?;

            let inner = if is_https {
                let tls_start = Instant::now();
                let server_name = ServerName::try_from(host.clone())
                    .map_err(|e| io::Error::other(format!("invalid server name: {e}")))?;
                let tls_stream = tls.connect(server_name, stream).await?;
                tls_ms.record(tls_start.elapsed().as_millis() as f64);
                MaybeTls::Tls(Box::new(tls_stream))
            } else {
                MaybeTls::Plain(stream)
            };

            let alpn_h2 = match &inner {
                MaybeTls::Tls(s) => {
                    let (_io, conn): (&TcpStream, &rustls::ClientConnection) = s.get_ref();
                    conn.alpn_protocol() == Some(b"h2")
                }
                MaybeTls::Plain(_) => false,
            };

            let tracked = TrackedStream {
                inner,
                _guard: AliveGuard::new(pool_stats),
                alpn_h2,
            };
            Ok(TokioIo::new(tracked))
        })
    }
}

async fn connect_tcp(
    resolver: &HickoryDnsResolver,
    host: &str,
    port: u16,
    connect_timeout: Duration,
    keepalive: Option<Duration>,
    nodelay: bool,
    connect_ms: &Histogram,
) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = resolver
        .resolve_addrs(host)
        .await
        .map_err(io::Error::other)?
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect();

    if addrs.is_empty() {
        return Err(io::Error::other(format!("no addresses for {host}")));
    }

    let tcp_start = Instant::now();
    let stream = tokio::time::timeout(connect_timeout, async {
        let mut last_err: Option<io::Error> = None;
        for addr in &addrs {
            match TcpStream::connect(addr).await {
                Ok(s) => return Ok::<TcpStream, io::Error>(s),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::other("no addresses")))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "tcp connect timeout"))??;

    connect_ms.record(tcp_start.elapsed().as_millis() as f64);
    if nodelay {
        let _ = stream.set_nodelay(true);
    }
    if let Some(d) = keepalive {
        let sock = socket2::SockRef::from(&stream);
        let mut ka = socket2::TcpKeepalive::new().with_time(d);
        ka = ka.with_interval(d);
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

impl AsyncRead for TrackedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.inner {
            MaybeTls::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TrackedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.inner {
            MaybeTls::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.inner {
            MaybeTls::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.inner {
            MaybeTls::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
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
        match &mut self.inner {
            MaybeTls::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }
}
