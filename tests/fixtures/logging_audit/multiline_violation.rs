// Test-the-test fixture: a non-compliant call that rustfmt has split across
// lines. The audit MUST flag this even though the macro and the offending
// value are on different lines. NOT compiled — scanned by ast-grep only.

fn emit(peer: std::net::SocketAddr) {
    tracing::warn!(
        client = %peer
            .ip(),
        "rustfmt split this; a line-oriented grep would miss it"
    );
}
