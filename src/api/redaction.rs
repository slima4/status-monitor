use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::domain::{CheckSpec, NotificationChannel, Target};

/// Wire-level placeholder substituted for populated credentials in API responses.
/// Re-submitting it on `PATCH` is rejected so a `GET → PATCH` round-trip cannot
/// silently overwrite the real value with the sentinel.
pub const REDACTED: &str = "***";

/// In-place credential redaction. Implemented by API-layer wrappers so callers
/// can't serialize a `Target` to a client without going through `Redacted<T>`.
pub trait RedactInPlace {
    fn redact_in_place(&mut self);
}

impl RedactInPlace for Target {
    fn redact_in_place(&mut self) {
        redact_check(&mut self.check);
    }
}

impl RedactInPlace for NotificationChannel {
    fn redact_in_place(&mut self) {
        self.config.redact_in_place();
    }
}

impl<T: RedactInPlace> RedactInPlace for Vec<T> {
    fn redact_in_place(&mut self) {
        for item in self {
            item.redact_in_place();
        }
    }
}

fn redact_check(check: &mut CheckSpec) {
    if let CheckSpec::Http(http) = check {
        if let Some((u, p)) = http.basic_auth.as_mut() {
            *u = REDACTED.to_string();
            *p = REDACTED.to_string();
        }
        if let Some(token) = http.bearer_token.as_mut() {
            *token = REDACTED.to_string();
        }
    }
}

/// Stronger redaction for surfaces shown to people outside the owning org (the
/// public `/m/{token}` share view). The operator API only masks the structured
/// credential fields, trusting a member with the rest of the config; a public
/// viewer must not see anything that can carry a secret. On top of
/// [`redact_check`] this masks every request-header value and the request body
/// (common homes for `Authorization` / `X-Api-Key` / `Cookie` secrets) and
/// strips credentials embedded in the URL (userinfo, query, fragment). Mask the
/// whole class rather than allow-list "known sensitive" names — a denylist on a
/// public surface is the wrong default.
pub(crate) fn redact_check_for_public(check: &mut CheckSpec) {
    redact_check(check);
    if let CheckSpec::Http(http) = check {
        for value in http.headers.values_mut() {
            *value = REDACTED.to_string();
        }
        if http.body.is_some() {
            http.body = Some(REDACTED.to_string());
        }
        let _ = http.url.set_username("");
        let _ = http.url.set_password(None);
        http.url.set_query(None);
        http.url.set_fragment(None);
    }
}

/// Response wrapper that redacts credential fields before serialization. The
/// inner value is private so the only path from a `Target` (or `Vec<Target>`)
/// to JSON in a handler runs through `IntoResponse`, enforcing redaction at
/// the type level.
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }
}

impl<T> IntoResponse for Redacted<T>
where
    T: RedactInPlace + Serialize,
{
    fn into_response(self) -> Response {
        let Self(mut inner) = self;
        inner.redact_in_place();
        Json(inner).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use crate::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod};

    use super::{REDACTED, redact_check_for_public};

    fn http_with_secrets() -> CheckSpec {
        CheckSpec::Http(HttpCheck {
            url: url::Url::parse("https://user:pass@api.example.com/health?token=qsecret#frag")
                .unwrap(),
            method: HttpMethod::Get,
            timeout: Duration::from_secs(5),
            follow_redirects: true,
            max_redirects: 3,
            expected_status: ExpectedStatus::Exact(200),
            expected_body_contains: None,
            headers: HashMap::from([("X-Api-Key".to_string(), "hsecret".to_string())]),
            body: Some("psecret".to_string()),
            verify_tls: true,
            basic_auth: Some(("u".into(), "p".into())),
            bearer_token: Some("bsecret".into()),
        })
    }

    #[test]
    fn public_redaction_masks_every_secret_bearing_field() {
        let mut check = http_with_secrets();
        redact_check_for_public(&mut check);
        let CheckSpec::Http(http) = check else {
            panic!("expected http");
        };
        // Structured credentials.
        assert_eq!(http.bearer_token.as_deref(), Some(REDACTED));
        assert_eq!(
            http.basic_auth,
            Some((REDACTED.to_string(), REDACTED.to_string()))
        );
        // Header values + body — common secret homes.
        assert_eq!(
            http.headers.get("X-Api-Key").map(String::as_str),
            Some(REDACTED)
        );
        assert_eq!(http.body.as_deref(), Some(REDACTED));
        // URL userinfo / query / fragment stripped; host + path kept.
        let rendered = http.url.to_string();
        assert!(
            !rendered.contains("secret"),
            "url leaked a secret: {rendered}"
        );
        assert!(!rendered.contains('@'), "userinfo not stripped: {rendered}");
        assert_eq!(rendered, "https://api.example.com/health");
    }
}
