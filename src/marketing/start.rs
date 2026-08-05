//! `GET /start?url=…` — where the hero's "monitor my site" form posts.
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

#[derive(Debug, Default, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    url: Option<String>,
}

pub async fn start(State(cfg): State<Arc<MarketingCfg>>, Query(q): Query<StartQuery>) -> Response {
    Redirect::to(&login_url(&cfg.app_url, q.url.as_deref())).into_response()
}

/// An unusable value falls through to plain sign-in: they came here to start,
/// and the app can ask again.
fn login_url(app_url: &str, typed: Option<&str>) -> String {
    let app = app_url.trim_end_matches('/');
    match typed.and_then(monitor_url) {
        Some(url) => {
            let after = format!("/targets/new?kind=http&url={}", encode(&url));
            format!("{app}/login?redirect_after={}", encode(&after))
        }
        None => format!("{app}/login"),
    }
}

/// Only the two web schemes survive, so nothing else can be smuggled into the
/// create form. Not shared with `targets_form::parse_monitor_url`: this module
/// may not import app code, and this copy is stricter because it screens
/// anonymous input.
fn monitor_url(raw: &str) -> Option<String> {
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
    Some(url.into())
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
            login_url(APP, Some("acme.com")),
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
                login_url(APP, Some(raw)),
                "https://app.uptimepage.dev/login",
                "{raw}"
            );
        }
        assert_eq!(login_url(APP, None), "https://app.uptimepage.dev/login");
    }

    #[test]
    fn path_and_query_survive_the_round_trip() {
        let out = login_url(APP, Some("http://acme.com/health?deep=1"));
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
