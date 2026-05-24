//! Static asset serving with content-addressed cache busting.
//!
//! One source of truth: [`url`] maps a logical path (`css/app.css`) to its
//! cache-busting URL (`/static/css/app.css?v=<hash>`), where `<hash>` is a
//! prefix of the file's SHA-256. The hash changes iff the bytes change —
//! that is what makes the `immutable` response header *truthful* rather
//! than a year-long promise the server can't keep.
//!
//! Templates must reference assets only through the `asset` askama filter
//! (`{{ "css/app.css"|asset }}`, registered in `crate::web::filters`).
//! A raw `/static/...` literal in a template is a bug the
//! `no_raw_static_refs_in_templates` test fails on, so cache-busting
//! can never silently regress.
//!
//! In release builds `rust_embed` bakes the files into the binary, so the
//! fingerprint is stable for that binary's lifetime. In debug builds it
//! reads from disk; the fingerprint map is computed once per process, so an
//! asset edit needs a restart to re-hash (cargo-watch already restarts).

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::extract::{Path, RawQuery};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

/// Static assets embedded into the release binary. In debug builds
/// rust-embed reads from the filesystem so edits show up without rebuilding.
#[derive(Embed)]
#[folder = "static/"]
struct StaticAssets;

/// Logical path → 8-hex content fingerprint, built once from the embedded
/// (release) or on-disk (debug) bytes.
static FINGERPRINTS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    StaticAssets::iter()
        .filter_map(|p| {
            // rust-embed already SHA-256s every file (baked in release, read
            // from disk in debug) — reuse that instead of a second hash
            // pass. 4 bytes is plenty: it only has to change when the bytes
            // do, not resist collision, for the handful of bundled assets.
            let hash = StaticAssets::get(&p)?.metadata.sha256_hash();
            Some((p.to_string(), hex::encode(&hash[..4])))
        })
        .collect()
});

/// The canonical way to reference a static asset. Returns a version-pinned
/// URL when the asset is known; otherwise the bare path (a newly added
/// asset in debug before the map is built — still serves, just not
/// cache-busted, and never reached for template-referenced assets thanks to
/// the drift test).
pub fn url(path: &str) -> String {
    match FINGERPRINTS.get(path) {
        Some(fp) => format!("/static/{path}?v={fp}"),
        None => format!("/static/{path}"),
    }
}

pub async fn serve(Path(path): Path<String>, RawQuery(query): RawQuery) -> Response {
    let Some(content) = StaticAssets::get(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let mime = mime_guess::from_path(&path).first_or_octet_stream();

    // `immutable` is only honest when the URL is version-pinned: a pinned
    // URL is requested again only if its bytes are unchanged. A bare URL
    // (hand-typed, or an old bookmark) gets a short revalidating cache so a
    // content change can't be hidden for a year.
    let versioned = query
        .as_deref()
        .is_some_and(|q| q.split('&').any(|kv| kv == "v" || kv.starts_with("v=")));
    let cache_control = if versioned {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=300"
    };

    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_owned()),
            (header::CACHE_CONTROL, cache_control.to_owned()),
        ],
        content.data,
    )
        .into_response()
}

/// Mount the fingerprinted static-asset route on the given router. Two
/// callers need an identical declaration — the operator app router and
/// the marketing router — and they must agree on the path and handler so
/// `{{ "css/app.css"|asset }}` resolves on both hosts. Funnel both
/// through here so the path can never drift.
pub fn mount_static<S>(router: axum::Router<S>) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    use axum::routing::get;
    router.route("/static/{*path}", get(serve))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn url_is_version_pinned_for_known_assets() {
        let u = url("css/app.css");
        assert!(
            u.starts_with("/static/css/app.css?v="),
            "expected a ?v= fingerprint, got {u}"
        );
        // Same bytes → same fingerprint (stable across calls).
        assert_eq!(u, url("css/app.css"));
    }

    #[test]
    fn url_unknown_asset_falls_back_to_bare_path() {
        assert_eq!(url("does/not/exist.js"), "/static/does/not/exist.js");
    }

    #[tokio::test]
    async fn versioned_request_is_immutable() {
        let resp = serve(
            Path("css/app.css".into()),
            RawQuery(Some("v=deadbeef".into())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/css",
        );
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable",
        );
        let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("tailwindcss"));
    }

    #[tokio::test]
    async fn unversioned_request_is_short_lived() {
        let resp = serve(Path("css/app.css".into()), RawQuery(None)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=300",
        );
    }

    #[tokio::test]
    async fn serves_htmx_js() {
        let resp = serve(Path("js/htmx.min.js".into()), RawQuery(None)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let mime = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(mime.starts_with("text/javascript") || mime.starts_with("application/javascript"));
    }

    #[tokio::test]
    async fn missing_asset_returns_404() {
        let resp = serve(Path("does/not/exist".into()), RawQuery(None)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Cache-busting only works if every template asset reference goes
    /// through the `asset` filter. A raw `/static/...` literal bypasses the
    /// fingerprint, so fail loudly the moment one appears.
    #[test]
    fn no_raw_static_refs_in_templates() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/templates");
        let mut offenders = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read templates dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("html") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read template");
                if body.contains("/static/") {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "templates must reference assets via the `asset` filter, not a \
             raw /static/ path. Offenders: {offenders:?}"
        );
    }
}
