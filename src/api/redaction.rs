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
