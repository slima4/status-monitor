use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    page_name: &str,
    confirm_url: &str,
    expires_hours: u32,
    unsubscribe_url: &str,
) -> RenderedEmail {
    let subject = format!("Confirm your subscription to {page_name}");

    let text_body = format!(
        "You asked to receive status updates for {page_name} on {site_name}.\n\
         \n\
         Confirm this address to start receiving notifications:\n\
         \n  {confirm_url}\n\
         \n\
         This link expires in {expires_hours} hours and can only be used once.\n\
         If you didn't request this, you can remove this address in one click:\n\
         \n  {unsubscribe_url}\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Confirm your subscription to {page_esc}</h2>\n\
         <p>You asked to receive status updates for <strong>{page_esc}</strong>.</p>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{url_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">Confirm subscription</a>\n\
         </p>\n\
         <p style=\"font-size:0.9em;color:#555;\">This link expires in <strong>{expires_hours}</strong> hours and can only be used once.</p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">If you didn't request this, <a href=\"{unsub_attr}\" style=\"color:#888;\">remove this address</a> and no updates will be sent to it.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        page_esc = html_escape(page_name),
        url_attr = html_escape(confirm_url),
        unsub_attr = html_escape(unsubscribe_url),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
