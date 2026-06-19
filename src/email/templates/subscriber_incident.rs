use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

/// Capitalised phase label ("investigating" -> "Investigating").
fn phase_label(phase: &str) -> String {
    let mut chars = phase.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn render(
    page_name: &str,
    incident_title: &str,
    phase: &str,
    message: &str,
    incident_url: &str,
    unsubscribe_url: &str,
) -> RenderedEmail {
    let label = phase_label(phase);
    let subject = format!("[{page_name}] {incident_title} — {label}");

    let text_body = format!(
        "{incident_title}\n\
         Status: {label}\n\
         \n\
         {message}\n\
         \n\
         View the status page:\n  {incident_url}\n\
         \n\
         Unsubscribe:\n  {unsubscribe_url}\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">{title_esc}</h2>\n\
         <p style=\"color:#555;\">Status: <strong>{label_esc}</strong></p>\n\
         <p style=\"white-space:pre-wrap;\">{message_esc}</p>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{url_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">View status page</a>\n\
         </p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">You're receiving this because you subscribed to {page_esc}. <a href=\"{unsub_attr}\">Unsubscribe</a>.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        title_esc = html_escape(incident_title),
        label_esc = html_escape(&label),
        message_esc = html_escape(message),
        url_attr = html_escape(incident_url),
        page_esc = html_escape(page_name),
        unsub_attr = html_escape(unsubscribe_url),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
