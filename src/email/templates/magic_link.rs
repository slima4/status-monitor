//! Magic-link sign-in template. Schema + renderer land now so the
//! `EmailTemplate` enum and the `magic_link_tokens` table are wire-compatible
//! the day the request/verify endpoints get wired and
//! `auth.enabled_methods` is extended with `"magic_link"`. No template
//! change required at that point.

use crate::email::templates::html_escape;
use crate::email::templates::layout::{self, ButtonStyle, Page};
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    url: &str,
    expires_in_minutes: u32,
    ip_hint: Option<&str>,
) -> RenderedEmail {
    let subject = format!("Sign in to {site_name}");
    let ip_line_text = ip_hint
        .map(|ip| format!("\nSign-in requested from {ip}.\n"))
        .unwrap_or_default();

    let text_body = format!(
        "Click the link below to sign in to {site_name}:\n\
         \n  {url}\n\
         \n\
         This link expires in {expires_in_minutes} minutes and can only be used once.\n\
         {ip_line_text}\
         If you didn't request this, you can ignore the message.\n"
    );

    let mut body = layout::button(url, "Sign in", ButtonStyle::Solid);
    body.push_str(&layout::fine_print(&format!(
        "This link expires in <strong>{expires_in_minutes}</strong> minutes and can only be \
         used once."
    )));
    if let Some(ip) = ip_hint {
        body.push_str(&layout::fine_print(&format!(
            "Sign-in requested from {}.",
            html_escape(ip)
        )));
    }

    let html_body = layout::render(Page {
        title: &subject,
        preheader: &format!("One-time sign-in link, good for {expires_in_minutes} minutes."),
        site_name,
        header: layout::wordmark(site_name, "Sign in"),
        body,
        footnote: Some(layout::fine_print(
            "If you didn't request this, you can ignore the message.",
        )),
    });

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
