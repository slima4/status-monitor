//! One-shot flash cookie: a short-lived, server-set signal that a banner
//! should render exactly once on the next page load. Unlike a query param it
//! can't be forged by editing the URL and doesn't linger across refreshes or
//! get shared in a copied link.

use tower_cookies::Cookie;
use tower_cookies::Cookies;
use tower_cookies::cookie::SameSite;
use tower_cookies::cookie::time::Duration;

const COOKIE_NAME: &str = "_sm_flash";
// Long enough to survive the post-login redirect chain, short enough that it
// never resurfaces on a later visit if the consuming page is somehow skipped.
const TTL_SECS: i64 = 60;

/// Flags carried by a single flash. Extend as new one-shot banners appear.
#[derive(Debug, Default, Clone, Copy)]
pub struct Flash {
    pub restored: bool,
    pub invite_missed: bool,
}

impl Flash {
    fn is_empty(self) -> bool {
        !self.restored && !self.invite_missed
    }

    fn encode(self) -> String {
        let mut tags = Vec::new();
        if self.restored {
            tags.push("restored");
        }
        if self.invite_missed {
            tags.push("invite_missed");
        }
        tags.join(",")
    }

    fn decode(value: &str) -> Self {
        let mut f = Self::default();
        for tag in value.split(',') {
            match tag.trim() {
                "restored" => f.restored = true,
                "invite_missed" => f.invite_missed = true,
                _ => {}
            }
        }
        f
    }
}

/// Stage a flash for the next page load. No-op when nothing is set. `domain`
/// must match the session cookie's (empty = host-only); the post-login
/// redirect can cross to another subdomain, so a host-only flash would be
/// dropped where the domain-scoped session survives.
pub fn set(cookies: &Cookies, flash: Flash, secure: bool, domain: &str) {
    if flash.is_empty() {
        return;
    }
    let mut c = Cookie::new(COOKIE_NAME, flash.encode());
    c.set_http_only(true);
    c.set_secure(secure);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    if !domain.is_empty() {
        c.set_domain(domain.to_owned());
    }
    c.set_max_age(Duration::seconds(TTL_SECS));
    cookies.add(c);
}

/// Read and consume the flash: returns the flags and clears the cookie so the
/// banner fires exactly once. `domain` must mirror [`set`] — a removal cookie
/// with a mismatched path/domain leaves the browser's copy in place and the
/// banner repeats.
pub fn take(cookies: &Cookies, domain: &str) -> Flash {
    let Some(c) = cookies.get(COOKIE_NAME) else {
        return Flash::default();
    };
    let flash = Flash::decode(c.value());
    let mut gone = Cookie::new(COOKIE_NAME, "");
    gone.set_path("/");
    if !domain.is_empty() {
        gone.set_domain(domain.to_owned());
    }
    cookies.remove(gone);
    flash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_flags_through_encode_decode() {
        let f = Flash {
            restored: true,
            invite_missed: true,
        };
        let back = Flash::decode(&f.encode());
        assert!(back.restored && back.invite_missed);

        let only = Flash {
            restored: true,
            invite_missed: false,
        };
        let back = Flash::decode(&only.encode());
        assert!(back.restored && !back.invite_missed);
    }

    #[test]
    fn unknown_tags_are_ignored() {
        let f = Flash::decode("restored,bogus,");
        assert!(f.restored && !f.invite_missed);
    }
}
