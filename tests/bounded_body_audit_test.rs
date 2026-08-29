//! Every `Limited::new(..).collect()` must sit inside a `tokio::time` bound.
//!
//! The shape accumulated in eight places before anyone noticed, which is why
//! the guard is a lint rather than a test per call site.

use std::path::Path;
use std::process::Command;

const RULE: &str = "scripts/sg-rules/bounded_body_reads.yml";

/// True when ast-grep reports the path clean. Panics — never skips — if the
/// binary is missing: a green-because-unenforced audit is the failure this
/// exists to prevent.
fn scan_clean(path: &str) -> bool {
    let root = env!("CARGO_MANIFEST_DIR");
    assert!(
        Path::new(root).join(RULE).exists(),
        "rule file {RULE} missing"
    );
    let output = Command::new("ast-grep")
        .args(["scan", "--rule", RULE, path])
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "ast-grep not runnable ({e}); install it (`brew install ast-grep`, \
                 or the release asset used in CI). This audit must be enforced, \
                 not skipped."
            )
        });
    output.status.success()
}

#[test]
fn every_body_read_is_time_bounded() {
    assert!(
        scan_clean("src/"),
        "a `Limited::new(..).collect()` is not inside a `tokio::time` bound. \
         Share the deadline with the header phase (see `http_outbound::read_body_within`); \
         starting a second one doubles the worst case rather than capping it."
    );
}

#[test]
fn audit_catches_its_non_compliant_fixture() {
    assert!(
        !scan_clean("tests/fixtures/bounded_body/unbounded_violation.rs"),
        "rule no longer catches a plain unbounded collect"
    );
    assert!(
        scan_clean("tests/fixtures/bounded_body/bounded_ok.rs"),
        "rule now flags a correctly bounded read"
    );
}
