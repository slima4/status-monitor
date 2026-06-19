use chrono::{DateTime, Utc};

use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

fn fmt(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    page_name: &str,
    title: &str,
    description: Option<&str>,
    phase: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    page_url: &str,
    unsubscribe_url: &str,
) -> RenderedEmail {
    let completed = phase == "completed";
    let heading = if completed {
        "Maintenance completed"
    } else {
        "Scheduled maintenance"
    };
    let subject = format!("[{page_name}] {heading}: {title}");
    let window = format!("{} — {}", fmt(starts_at), fmt(ends_at));
    let desc_text = description.map(|d| format!("\n{d}\n")).unwrap_or_default();

    let text_body = format!(
        "{title}\n\
         {heading}\n\
         When: {window}\n\
         {desc_text}\n\
         View the status page:\n  {page_url}\n\
         \n\
         Unsubscribe:\n  {unsubscribe_url}\n"
    );

    let desc_html = description
        .map(|d| format!("<p style=\"white-space:pre-wrap;\">{}</p>", html_escape(d)))
        .unwrap_or_default();
    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">{title_esc}</h2>\n\
         <p style=\"color:#555;\">{heading} · <strong>{window_esc}</strong></p>\n\
         {desc_html}\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{url_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">View status page</a>\n\
         </p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">You're receiving this because you subscribed to {page_esc}. <a href=\"{unsub_attr}\">Unsubscribe</a>.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        title_esc = html_escape(title),
        window_esc = html_escape(&window),
        url_attr = html_escape(page_url),
        page_esc = html_escape(page_name),
        unsub_attr = html_escape(unsubscribe_url),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
