//! CI invariant: the marketing module must not touch storage / domain /
//! tenancy / app state. Belt-and-braces regex scan of `src/marketing/`.
//! A behavioural counterpart (every route serves a 2xx with no DB pool)
//! lives in `marketing_no_db_test.rs`.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_PATHS: &[&str] = &[
    "crate::storage",
    "crate::domain",
    "crate::tenancy",
    "crate::state",
    "AppState",
];

fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(root).expect("read marketing dir") {
        let path = e.expect("dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn marketing_module_does_not_import_app_internals() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/marketing");
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    assert!(!files.is_empty(), "no marketing source files found");

    let mut offenders: Vec<(String, String, &'static str)> = Vec::new();
    for path in files {
        let body = fs::read_to_string(&path).expect("read marketing source");
        for (lineno, line) in body.lines().enumerate() {
            // Strip the line comment portion so the rule itself can name
            // the forbidden symbols in prose (this file does).
            let code = line.split("//").next().unwrap_or(line);
            for needle in FORBIDDEN_PATHS {
                if code.contains(needle) {
                    offenders.push((
                        format!("{}:{}", path.display(), lineno + 1),
                        line.trim().to_string(),
                        needle,
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "marketing module must not reference app internals:\n{}",
        offenders
            .iter()
            .map(|(at, line, needle)| format!("  {at} ({needle})\n    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
