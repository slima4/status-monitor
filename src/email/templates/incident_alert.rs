use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

/// Alert delivery rides the transactional sender: the plain text the chat
/// transports send becomes the body; its first line is the subject.
pub fn render(site_name: &str, body: &str) -> RenderedEmail {
    let subject = body.lines().next().unwrap_or("incident alert").to_string();
    let text_body = format!("{body}\n\n— {site_name} alerts\n");
    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <pre style=\"font-family:ui-monospace,monospace;white-space:pre-wrap;\">{body_esc}</pre>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">— {site_esc} alerts</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        body_esc = html_escape(body),
        site_esc = html_escape(site_name),
    );
    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
