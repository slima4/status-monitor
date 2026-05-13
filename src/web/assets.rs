use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

/// Static assets embedded into the release binary. In debug builds
/// rust-embed reads from the filesystem so edits show up without rebuilding.
#[derive(Embed)]
#[folder = "static/"]
struct StaticAssets;

pub async fn serve(Path(path): Path<String>) -> Response {
    let Some(content) = StaticAssets::get(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let cache_control = if path.starts_with("css/") || path.starts_with("js/") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn serves_tailwind_css() {
        let resp = serve(Path("css/app.css".into())).await;
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
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("tailwindcss"));
    }

    #[tokio::test]
    async fn serves_htmx_js() {
        let resp = serve(Path("js/htmx.min.js".into())).await;
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
        let resp = serve(Path("does/not/exist".into())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
