//! Form-encoded URL component escaper. Single owner so the OAuth authorize
//! URL, the invitation accept/decline links, and the post-OAuth redirect
//! query string all agree on encoding (`+` for space, `%2F` for `/`, etc.).

/// Percent-encodes per `application/x-www-form-urlencoded` rules. The byte
/// stream comes from `url::form_urlencoded::byte_serialize`, so spaces
/// become `+` (not `%20`) — matches what `Location` headers and GitHub's
/// OAuth flow expect.
pub fn url_encode(s: &str) -> String {
    use url::form_urlencoded::byte_serialize;
    byte_serialize(s.as_bytes()).collect()
}

/// Append `key=value` to a query-string buffer, inserting `&` before all but
/// the first pair and form-encoding the value. Single owner so the various
/// list-view link builders assemble query strings identically.
pub fn push_param(buf: &mut String, key: &str, value: &str) {
    if !buf.is_empty() {
        buf.push('&');
    }
    buf.push_str(key);
    buf.push('=');
    buf.push_str(&url_encode(value));
}

/// Assemble an `application/x-www-form-urlencoded` body (or query tail)
/// from key/value pairs — one owner for the OAuth token-exchange payloads.
pub fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut buf = String::new();
    for (k, v) in pairs {
        push_param(&mut buf, k, v);
    }
    buf
}

/// Same-origin path predicate for `?redirect_after=` style query params.
/// Accepts `/foo`, `/foo/bar`, `/`; rejects `//evil.test`, `/\evil.test`,
/// `https://evil.test`, the empty string. Centralised so the login page link,
/// the OAuth callback final redirect, and any future redirect-honoring
/// surfaces agree on the policy.
pub fn safe_redirect_target(raw: &str) -> Option<&str> {
    let s = raw.trim();
    if !s.starts_with('/') {
        return None;
    }
    let rest = &s[1..];
    if rest.starts_with('/') || rest.starts_with('\\') {
        return None;
    }
    Some(s)
}

/// Build a tokenised action link `<base><path>?token=<urlencoded>`. `base` is
/// the configured public base URL (trailing slash tolerated); `path` is an
/// absolute path beginning with `/`. Single owner so the magic-link,
/// invitation, and account-recovery links all trim and encode identically.
pub fn token_link(base: &str, path: &str, token: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}{path}?token={}", url_encode(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_link_trims_base_and_encodes_token() {
        assert_eq!(
            token_link("https://x.test/", "/invitations/accept", "a b/c"),
            "https://x.test/invitations/accept?token=a+b%2Fc"
        );
        assert_eq!(
            token_link("https://x.test", "/auth/magic-link/verify", "tok"),
            "https://x.test/auth/magic-link/verify?token=tok"
        );
    }

    #[test]
    fn safe_redirect_target_accepts_same_origin_paths() {
        assert_eq!(safe_redirect_target("/"), Some("/"));
        assert_eq!(safe_redirect_target("/foo"), Some("/foo"));
        assert_eq!(safe_redirect_target("/foo/bar?x=1"), Some("/foo/bar?x=1"));
    }

    #[test]
    fn safe_redirect_target_rejects_scheme_and_protocol_relative() {
        assert!(safe_redirect_target("").is_none());
        assert!(safe_redirect_target("//evil.test").is_none());
        assert!(safe_redirect_target("/\\evil.test").is_none());
        assert!(safe_redirect_target("https://evil.test").is_none());
        assert!(safe_redirect_target("javascript:alert(1)").is_none());
    }
}
