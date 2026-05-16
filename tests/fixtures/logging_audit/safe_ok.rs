// Test-the-test fixture: the same `%ip` shape, but with the inline `// SAFE:`
// escape hatch. The audit MUST NOT flag this — proving the escape hatch
// works, so a future change that breaks it also fails CI. NOT compiled.

fn emit(sock: std::net::SocketAddr) {
    let ip = sock.ip().to_string();
    tracing::info!(
        // SAFE: this fixture asserts the escape hatch is honoured
        %ip,
        "annotated as reviewed"
    );
}
