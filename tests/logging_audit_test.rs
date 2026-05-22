//! Privacy-friendly-logging audit. Runs the structural,
//! value-aware `ast-grep` rule from a `#[test]` so CI fails on a hit, and
//! "tests the test": deliberately non-compliant multi-line + local-binding
//! fixtures MUST still be caught, so a future weakening of the pattern fails
//! here rather than silently shipping raw IPs / PII to the logs.

use std::path::Path;
use std::process::Command;

const RULE: &str = "scripts/sg-rules/no_unredacted_pii_in_logs.yml";

/// Run `ast-grep scan --rule RULE <path>` from the repo root. Returns true
/// when ast-grep reports the code clean (exit 0), false when it found a
/// violation (exit 1). Panics — never skips — if the binary is missing: a
/// green-because-unenforced audit is exactly the failure this guards.
fn scan_clean(path: &str) -> bool {
    let root = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("ast-grep")
        .args(["scan", "--rule", RULE, path])
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "ast-grep not runnable ({e}); install it (`brew install ast-grep`, \
                 or the release asset used in CI). The logging audit must be \
                 enforced, not skipped."
            )
        });
    assert!(
        Path::new(root).join(RULE).exists(),
        "rule file {RULE} missing"
    );
    output.status.success()
}

#[test]
fn no_unredacted_pii_in_log_call_sites() {
    assert!(
        scan_clean("src/"),
        "ast-grep found a tracing/log call interpolating a raw IP / email / \
         URL / slug / token / secret. Hash or redact it, or add an inline \
         `// SAFE: <reason>` comment inside the macro call."
    );
}

#[test]
fn audit_catches_its_non_compliant_fixtures() {
    // Test-the-test: both must be flagged. If either passes, the rule has
    // been weakened (e.g. no longer line-break-proof, or lost the
    // local-binding case) and this fails CI.
    assert!(
        !scan_clean("tests/fixtures/logging_audit/multiline_violation.rs"),
        "rule no longer catches the rustfmt-split multi-line violation"
    );
    assert!(
        !scan_clean("tests/fixtures/logging_audit/local_binding_violation.rs"),
        "rule no longer catches the `let ip = …; info!(%ip)` local-binding violation"
    );
    // And the escape hatch must still exempt an annotated call.
    assert!(
        scan_clean("tests/fixtures/logging_audit/safe_ok.rs"),
        "the `// SAFE:` escape hatch is no longer honoured"
    );
}
