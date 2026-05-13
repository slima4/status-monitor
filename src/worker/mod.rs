pub mod circuit_breaker;
pub mod http_check;
pub mod pool;
pub mod tcp_check;
pub mod tls_cert;

pub use http_check::execute_http_check;
pub use pool::{CheckTask, ResultFanout, WorkerPool};

use std::net::SocketAddr;

use tokio::net::TcpStream;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckSpec};
use crate::http_client::HttpClients;

/// Resolves `host`, filters the addresses through the shared SSRF guard, and
/// tries to open a TCP connection to `(ip, port)`. Used by TCP and TLS-cert
/// checks — both want exactly the same resolve-and-connect dance.
pub(crate) async fn connect_via_guard(
    host: &str,
    port: u16,
    clients: &HttpClients,
) -> anyhow::Result<TcpStream> {
    let guard = clients.ssrf_guard();
    let mut last_err: Option<std::io::Error> = None;
    let mut tried = false;
    for ip in clients.resolver().resolve_addrs(host).await? {
        if !guard.allow(ip) {
            continue;
        }
        tried = true;
        match TcpStream::connect(SocketAddr::new(ip, port)).await {
            Ok(s) => return Ok(s),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e.into());
    }
    if tried {
        Err(anyhow::anyhow!("no addresses for {host}"))
    } else {
        Err(anyhow::anyhow!("no allowed addresses for {host}"))
    }
}

pub async fn execute(target_id: Uuid, spec: &CheckSpec, clients: &HttpClients) -> CheckResult {
    match spec {
        CheckSpec::Http(http) => execute_http_check(target_id, http, clients).await,
        CheckSpec::Tcp(tcp) => tcp_check::execute_tcp_check(target_id, tcp, clients).await,
        CheckSpec::TlsCert(cert) => {
            tls_cert::execute_tls_cert_check(target_id, cert, clients).await
        }
    }
}
