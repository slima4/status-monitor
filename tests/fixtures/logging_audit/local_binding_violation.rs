// Test-the-test fixture: the prevailing `let ip = …; info!(%ip)` idiom that a
// type-name match (IpAddr/SocketAddr) would miss entirely. The audit MUST
// flag the `%ip` interpolation. NOT compiled — scanned by ast-grep only.

fn emit(sock: std::net::SocketAddr) {
    let ip = sock.ip().to_string();
    tracing::info!(%ip, "raw ip reached the log via a local binding");
}
