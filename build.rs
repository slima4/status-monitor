use std::path::Path;
use std::process::Command;

/// AGPL-3.0 §13: a user interacting with a modified version over a network
/// must be offered the Corresponding Source. The running binary therefore
/// has to know *which* source it was built from. Two values are baked in at
/// compile time so the footer link is correct for upstream builds, CI
/// builds, and downstream forks alike:
///
/// * `SM_SOURCE_COMMIT` — explicit env override (CI sets `$GITHUB_SHA`),
///   else `git rev-parse`, else `"unknown"` (source-tarball builds).
/// * `SM_SOURCE_URL` — explicit env override (a fork points this at its
///   own repo to stay §13-compliant), else the upstream repository.
fn emit_source_identity() {
    println!("cargo::rerun-if-env-changed=SM_SOURCE_COMMIT");
    println!("cargo::rerun-if-env-changed=SM_SOURCE_URL");
    watch_git_head();

    let commit = std::env::var("SM_SOURCE_COMMIT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        })
        // Empty (not a placeholder string) is the "no git context" signal —
        // the render side keys off emptiness, so no sentinel literal has to
        // stay in sync across the build/runtime boundary.
        .unwrap_or_default();

    let url = std::env::var("SM_SOURCE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://github.com/slima4/status-monitor".to_string());

    println!("cargo::rustc-env=SM_SOURCE_COMMIT={commit}");
    println!("cargo::rustc-env=SM_SOURCE_URL={url}");
}

/// Re-bake the commit whenever HEAD moves. `.git/HEAD` alone is not enough:
/// a commit on the *current* branch rewrites `.git/refs/heads/<branch>` (or
/// `.git/packed-refs`), not `.git/HEAD`, so the symref is resolved and the
/// underlying ref watched too. Only existing paths are emitted — a missing
/// `rerun-if-changed` target forces a rebuild every time, and non-git
/// builds (source tarball, worktree gitdir file) must just skip cleanly.
fn watch_git_head() {
    let git = Path::new(".git");
    let head = git.join("HEAD");
    if !head.exists() {
        return;
    }
    println!("cargo::rerun-if-changed=.git/HEAD");
    if git.join("packed-refs").exists() {
        println!("cargo::rerun-if-changed=.git/packed-refs");
    }
    if let Ok(h) = std::fs::read_to_string(&head)
        && let Some(rf) = h.strip_prefix("ref:").map(str::trim)
        && git.join(rf).exists()
    {
        println!("cargo::rerun-if-changed=.git/{rf}");
    }
}

fn main() {
    emit_source_identity();

    // Tailwind v4 scans `templates/**/*.html` AND `src/**/*.rs` for class
    // names (see @source directives in static/css/input.css), so the CSS
    // must rebuild whenever either tree changes — including Rust files
    // that emit class strings via format!(). The ~40ms overhead is the
    // cost of co-locating class names with the code that uses them.
    println!("cargo::rerun-if-changed=templates");
    println!("cargo::rerun-if-changed=static/css/input.css");
    println!("cargo::rerun-if-changed=src");
    println!("cargo::rerun-if-changed=scripts/fetch-tailwind.sh");

    if !Path::new("bin/tailwindcss").exists() {
        let fetch = Command::new("scripts/fetch-tailwind.sh")
            .status()
            .expect("failed to invoke scripts/fetch-tailwind.sh");
        assert!(fetch.success(), "fetch-tailwind.sh exited non-zero");
    }

    let status = Command::new("./bin/tailwindcss")
        .args([
            "--input",
            "static/css/input.css",
            "--output",
            "static/css/app.css",
            "--minify",
        ])
        .status()
        .expect("tailwind build failed — is ./bin/tailwindcss present?");
    assert!(status.success(), "tailwind exited non-zero");
}
