//! `GET /start?url=…&kind=…` — where the marketing forms post.
//! Normalises what was typed and hands it to the app's login, which carries
//! it through OAuth to a prefilled create form. The hop is server-side so the
//! form works with JavaScript off.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::config::MarketingCfg;

pub const START_PATH: &str = "/start";

/// Bounded because this ends up inside a `Location` header.
const MAX_URL_LEN: usize = 512;

/// Check kinds whose create form starts from a bare host. Anything else, and
/// anything unrecognised, is handed over as an http URL.
const HOST_KINDS: &[&str] = &["dns", "domain_expiry", "tls_cert", "tcp", "ping"];

#[derive(Debug, Default, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

pub async fn start(State(cfg): State<Arc<MarketingCfg>>, Query(q): Query<StartQuery>) -> Response {
    let target = login_url(&cfg.app_url, q.url.as_deref(), q.kind.as_deref());
    Redirect::to(&target).into_response()
}

/// An unusable value falls through to plain sign-in: they came here to start,
/// and the app can ask again.
fn login_url(app_url: &str, typed: Option<&str>, kind: Option<&str>) -> String {
    let app = app_url.trim_end_matches('/');
    let Some(url) = typed.and_then(monitor_url) else {
        return format!("{app}/login");
    };
    // An unknown kind is ignored rather than refused: it reaches the create
    // form as a plain http monitor, which is what the hero form sends anyway.
    let after = match kind.filter(|k| HOST_KINDS.contains(k)) {
        Some(kind) => format!(
            "/targets/new?kind={kind}&host={}",
            encode(url.host_str().unwrap_or_default())
        ),
        None => format!("/targets/new?kind=http&url={}", encode(url.as_str())),
    };
    format!("{app}/login?redirect_after={}", encode(&after))
}

/// Only the two web schemes survive, so nothing else can be smuggled into the
/// create form. Not shared with `targets_form::parse_monitor_url`: this module
/// may not import app code, and this copy is stricter because it screens
/// anonymous input.
fn monitor_url(raw: &str) -> Option<url::Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_LEN {
        return None;
    }
    let candidate = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    let url = url::Url::parse(&candidate).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    // A dotless host is a LAN name, a typo, or a pasted search phrase.
    if !host.contains('.') {
        return None;
    }
    Some(url)
}

/// Not `auth::url::url_encode`: this module may not import app code.
fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "https://app.uptimepage.dev/";

    #[test]
    fn bare_host_is_promoted_and_double_encoded() {
        assert_eq!(
            login_url(APP, Some("acme.com"), None),
            "https://app.uptimepage.dev/login?redirect_after=\
             %2Ftargets%2Fnew%3Fkind%3Dhttp%26url%3Dhttps%253A%252F%252Facme.com%252F"
        );
    }

    #[test]
    fn unusable_values_still_reach_sign_in() {
        for raw in [
            "",
            "   ",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "localhost",
            "how do i monitor a website",
            &"a".repeat(MAX_URL_LEN + 1),
        ] {
            assert_eq!(
                login_url(APP, Some(raw), None),
                "https://app.uptimepage.dev/login",
                "{raw}"
            );
        }
        assert_eq!(
            login_url(APP, None, None),
            "https://app.uptimepage.dev/login"
        );
    }

    #[test]
    fn a_host_kind_hands_over_the_host_alone() {
        let out = login_url(APP, Some("https://acme.com/ignored"), Some("dns"));
        assert!(
            out.ends_with("%2Ftargets%2Fnew%3Fkind%3Ddns%26host%3Dacme.com"),
            "{out}"
        );
        // Unknown and absent kinds both fall back to the http URL form.
        for kind in [Some("nonsense"), Some("http"), None] {
            let out = login_url(APP, Some("acme.com"), kind);
            assert!(out.contains("kind%3Dhttp%26url%3D"), "{kind:?} -> {out}");
        }
    }

    #[test]
    fn path_and_query_survive_the_round_trip() {
        let out = login_url(APP, Some("http://acme.com/health?deep=1"), None);
        let after = out.split("redirect_after=").nth(1).unwrap();
        let decoded: String = url::form_urlencoded::parse(after.as_bytes())
            .map(|(k, _)| k.into_owned())
            .collect();
        assert_eq!(
            decoded,
            "/targets/new?kind=http&url=http%3A%2F%2Facme.com%2Fhealth%3Fdeep%3D1"
        );
    }
}
