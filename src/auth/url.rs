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
