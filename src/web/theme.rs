//! `sm_theme` cookie helper. The cookie is a fast-path mirror of `users.theme`
//! that the inline boot script in `templates/base.html` reads BEFORE the
//! stylesheet parses, so the user's chosen theme is the very first render.
//! The DB column remains the source of truth.

use tower_cookies::Cookie;
use tower_cookies::Cookies;
use tower_cookies::cookie::SameSite;
use tower_cookies::cookie::time::Duration;

use crate::app::AppState;
use crate::domain::{AppTheme, UserId};
use crate::error::Result;
use crate::storage::users as users_store;

pub const COOKIE_NAME: &str = "sm_theme";

/// `http_only=false` is intentional — the inline boot script reads
/// `document.cookie` to apply the theme class before the stylesheet parses.
pub fn build_cookie(theme: AppTheme, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE_NAME, theme.as_str().to_owned());
    c.set_http_only(false);
    c.set_secure(secure);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_max_age(Duration::days(365));
    c
}

/// Read `users.theme` and set the cookie. Called from every login/session-issue
/// site so a fresh browser picks up the user's stored theme on first paint.
pub async fn issue_for(state: &AppState, cookies: &Cookies, user: UserId) -> Result<()> {
    let Some(pool) = state.db.as_ref() else {
        return Ok(());
    };
    let theme = users_store::get_theme(pool, user).await?;
    cookies.add(build_cookie(theme, state.cfg.auth.session.cookie_secure));
    Ok(())
}
