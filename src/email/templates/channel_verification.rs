use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    channel_name: &str,
    verify_url: &str,
    expires_hours: u32,
) -> RenderedEmail {
    let subject = format!("Verify this address for {site_name} alerts");

    let text_body = format!(
        "This address was added as the alert channel \"{channel_name}\" on {site_name}.\n\
         \n\
         Confirm it to start receiving alerts:\n\
         \n  {verify_url}\n\
         \n\
         This link expires in {expires_hours} hours and can only be used once.\n\
         If you didn't expect this, you can ignore the message — no alerts \
         will be sent to this address.\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Verify this address for {site_esc} alerts</h2>\n\
         <p>This address was added as the alert channel <strong>{channel_esc}</strong>.</p>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{url_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">Verify address</a>\n\
         </p>\n\
         <p style=\"font-size:0.9em;color:#555;\">This link expires in <strong>{expires_hours}</strong> hours and can only be used once.</p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">If you didn't expect this, you can ignore the message — no alerts will be sent to this address.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        site_esc = html_escape(site_name),
        channel_esc = html_escape(channel_name),
        url_attr = html_escape(verify_url),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
