//! Zero-copy JSON responder for cached `Arc<T>` payloads.
//!
//! `axum::Json<T>` takes `T` by value, so handing it an `Arc<T>` forces an
//! `Arc::unwrap_or_clone` — and because the in-process cache always retains
//! its own reference, that always lands on the deep-clone branch. `JsonArc`
//! serialises through `&*self.0` instead, so the page struct + its incident /
//! component / maintenance vecs + every owned `String` stay borrowed.

use std::sync::Arc;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub struct JsonArc<T>(pub Arc<T>);

const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

impl<T: Serialize> IntoResponse for JsonArc<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&*self.0) {
            Ok(buf) => ([(header::CONTENT_TYPE, APPLICATION_JSON)], buf).into_response(),
            Err(err) => {
                tracing::error!(error = %err, "JsonArc serialise failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "serialise error").into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Payload {
        name: String,
        n: u32,
    }

    #[tokio::test]
    async fn serialises_arc_payload_to_application_json() {
        let arc = Arc::new(Payload {
            name: "acme".into(),
            n: 42,
        });
        let resp = JsonArc(arc.clone()).into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["name"], "acme");
        assert_eq!(parsed["n"], 42);
        // Arc still uniquely owned by the test → no clone happened in the body.
        assert_eq!(Arc::strong_count(&arc), 1);
    }
}
