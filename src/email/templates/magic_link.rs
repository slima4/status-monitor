//! Magic-link sign-in template. Schema + renderer land now so the
//! `EmailTemplate` enum and the `magic_link_tokens` table are wire-compatible
//! the day the request/verify endpoints get wired and
//! `auth.enabled_methods` is extended with `"magic_link"`. No template
//! change required at that point.

use crate::email::templates::html_escape;
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
    let ip_line_html = ip_hint
        .map(|ip| format!("<p style=\"font-size:0.85em;color:#555;\">Sign-in requested from <code>{}</code>.</p>", html_escape(ip)))
        .unwrap_or_default();

    let text_body = format!(
        "Click the link below to sign in to {site_name}:\n\
         \n  {url}\n\
         \n\
         This link expires in {expires_in_minutes} minutes and can only be used once.\n\
         {ip_line_text}\
         If you didn't request this, you can ignore the message.\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Sign in to {site_esc}</h2>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{url_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">Sign in</a>\n\
         </p>\n\
         <p style=\"font-size:0.9em;color:#555;\">This link expires in <strong>{expires}</strong> minutes and can only be used once.</p>\n\
         {ip_line_html}\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">If you didn't request this, you can ignore the message.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        site_esc = html_escape(site_name),
        url_attr = html_escape(url),
        expires = expires_in_minutes,
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
