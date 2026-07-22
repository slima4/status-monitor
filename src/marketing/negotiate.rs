//! `Accept: text/markdown` serves the page's Markdown source; browsers
//! keep the HTML. Source, not an HTML-to-Markdown conversion: docs, posts
//! and the site index are authored in Markdown, so the agent-facing
//! representation is the original rather than a lossy round-trip.

use axum::http::header::{
    ACCEPT, CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH, VARY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use super::pages::{CachedRender, HTML_CONTENT_TYPE};

const MARKDOWN_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("text/markdown; charset=utf-8");
const VARY_ACCEPT: HeaderValue = HeaderValue::from_static("accept");
const NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");

/// Context-budget hint, mirroring Cloudflare's Markdown for Agents.
const TOKENS: HeaderName = HeaderName::from_static("x-markdown-tokens");

/// Serve whichever representation the client asked for. `Vary: Accept` is
/// stamped on both so a shared cache can never hand an agent's Markdown to
/// a browser, and each representation carries its own ETag.
pub(crate) fn serve(
    headers: &HeaderMap,
    html: &CachedRender,
    markdown: &CachedRender,
    cache_control: &HeaderValue,
) -> Response {
    let as_markdown = wants_markdown(headers);
    let repr = if as_markdown { markdown } else { html };

    if headers.get(IF_NONE_MATCH).is_some_and(|v| v == repr.etag) {
        return with_headers(
            StatusCode::NOT_MODIFIED,
            None,
            repr,
            cache_control,
            as_markdown,
        );
    }
    with_headers(
        StatusCode::OK,
        Some(repr.body.clone()),
        repr,
        cache_control,
        as_markdown,
    )
}

fn with_headers(
    status: StatusCode,
    body: Option<bytes::Bytes>,
    repr: &CachedRender,
    cache_control: &HeaderValue,
    as_markdown: bool,
) -> Response {
    let has_body = body.is_some();
    let mut resp = match body {
        Some(b) => (status, b).into_response(),
        None => status.into_response(),
    };
    let h = resp.headers_mut();
    h.insert(CACHE_CONTROL, cache_control.clone());
    h.insert(ETAG, repr.etag.clone());
    h.insert(VARY, VARY_ACCEPT);
    // A 304 carries no representation, so it carries no metadata about one.
    if !has_body {
        return resp;
    }
    if as_markdown {
        h.insert(CONTENT_TYPE, MARKDOWN_CONTENT_TYPE);
        // Blog Markdown is third-party input; keep a browser from sniffing
        // an embedded fragment back into HTML.
        h.insert(X_CONTENT_TYPE_OPTIONS, NOSNIFF);
        h.insert(TOKENS, estimate_tokens(repr.body.len()));
    } else {
        h.insert(CONTENT_TYPE, HTML_CONTENT_TYPE);
    }
    resp
}

/// Markdown only when it is asked for by name and outranks HTML. A browser
/// sends `text/html,…,*/*;q=0.8`, which must keep getting HTML.
fn wants_markdown(headers: &HeaderMap) -> bool {
    let Some(accept) = headers.get(ACCEPT).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let mut markdown = None::<f32>;
    let mut html = None::<f32>;
    for part in accept.split(',') {
        let mut fields = part.split(';');
        let media = fields.next().unwrap_or_default().trim();
        let q = fields
            .filter_map(|f| f.trim().strip_prefix("q="))
            .next()
            .and_then(|q| q.parse::<f32>().ok())
            .unwrap_or(1.0);
        let slot = if media.eq_ignore_ascii_case("text/markdown") {
            &mut markdown
        } else if media.eq_ignore_ascii_case("text/html")
            || media.eq_ignore_ascii_case("application/xhtml+xml")
        {
            &mut html
        } else {
            continue;
        };
        *slot = Some(slot.unwrap_or(q).max(q));
    }
    // A tie means the client named Markdown explicitly and ranked it level
    // with HTML — browsers never name it at all, so honour the request.
    match (markdown, html) {
        (Some(md), Some(html)) => md > 0.0 && md >= html,
        (Some(md), None) => md > 0.0,
        _ => false,
    }
}

/// Deliberately coarse: ~4 bytes per token, the same rule of thumb the
/// header exists to serve. Not a tokeniser.
fn estimate_tokens(bytes: usize) -> HeaderValue {
    HeaderValue::from(bytes.div_ceil(4) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn browser_accept_stays_on_html() {
        assert!(!wants_markdown(&accept(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,*/*;q=0.8"
        )));
        assert!(!wants_markdown(&accept("*/*")));
        assert!(!wants_markdown(&HeaderMap::new()));
    }

    #[test]
    fn explicit_markdown_wins() {
        assert!(wants_markdown(&accept("text/markdown")));
        assert!(wants_markdown(&accept("text/markdown, text/html;q=0.5")));
        assert!(wants_markdown(&accept("TEXT/MARKDOWN")));
    }

    #[test]
    fn html_wins_when_it_outranks_markdown() {
        assert!(!wants_markdown(&accept("text/markdown;q=0.2, text/html")));
        assert!(!wants_markdown(&accept("text/markdown;q=0")));
    }

    /// An agent that appends Markdown to a normal Accept list, rather than
    /// replacing it, still means it.
    #[test]
    fn an_equal_weight_tie_goes_to_markdown() {
        assert!(wants_markdown(&accept("text/markdown, text/html")));
        assert!(wants_markdown(&accept(
            "text/html;q=0.9, text/markdown;q=0.9"
        )));
    }

    #[test]
    fn not_modified_carries_no_representation_headers() {
        let repr = CachedRender {
            body: bytes::Bytes::from_static(b"# hi"),
            etag: HeaderValue::from_static("\"abc\""),
        };
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/markdown"));
        headers.insert(IF_NONE_MATCH, repr.etag.clone());
        let resp = serve(
            &headers,
            &repr,
            &repr,
            &HeaderValue::from_static("no-cache"),
        );

        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert!(!resp.headers().contains_key(CONTENT_TYPE));
        assert!(!resp.headers().contains_key(TOKENS));
        assert_eq!(resp.headers().get(ETAG).unwrap(), "\"abc\"");
        assert_eq!(resp.headers().get(VARY).unwrap(), "accept");
    }

    #[test]
    fn token_hint_counts_four_bytes_each() {
        assert_eq!(estimate_tokens(0), HeaderValue::from_static("0"));
        assert_eq!(estimate_tokens(9), HeaderValue::from_static("3"));
    }
}
