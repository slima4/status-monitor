use std::path::Path;
use std::process::Command;

fn main() {
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
