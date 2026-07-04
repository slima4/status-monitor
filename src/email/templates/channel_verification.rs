use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    channel_name: &str,
    verify_url: &str,
    expires_hours: u32,
    org_name: Option<&str>,
    decline_url: Option<&str>,
) -> RenderedEmail {
    let subject = format!("Verify this address for {site_name} alerts");
    let by_org = org_name.map(|o| format!(" by {o}")).unwrap_or_default();
    let decline_text = decline_url
        .map(|u| format!(", or block this address: {u}"))
        .unwrap_or_default();

    let text_body = format!(
        "This address was added as the alert channel \"{channel_name}\"{by_org} on {site_name}.\n\
         \n\
         Confirm it to start receiving alerts:\n\
         \n  {verify_url}\n\
         \n\
         This link expires in {expires_hours} hours and can only be used once.\n\
         If you didn't expect this, ignore this message{decline_text} — no alerts \
         are sent until it is confirmed.\n"
    );

    let by_org_html = org_name
        .map(|o| format!(" by <strong>{}</strong>", html_escape(o)))
        .unwrap_or_default();
    let decline_html = decline_url
        .map(|u| {
            format!(
                " Didn't expect this? <a href=\"{}\">Block this address</a>.",
                html_escape(u)
            )
        })
        .unwrap_or_default();

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Verify this address for {site_esc} alerts</h2>\n\
         <p>This address was added as the alert channel <strong>{channel_esc}</strong>{by_org_html}.</p>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{url_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">Verify address</a>\n\
         </p>\n\
         <p style=\"font-size:0.9em;color:#555;\">This link expires in <strong>{expires_hours}</strong> hours and can only be used once.</p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">No alerts are sent until this address is confirmed.{decline_html}</p>\n\
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

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn org_attribution_and_decline_link_appear() {
        let decline = "https://app/alert-channel/stop?c=1&t=2";
        let r = render(
            "Uptimepage",
            "Ops",
            "https://app/verify?token=t",
            24,
            Some("Acme Inc"),
            Some(decline),
        );
        assert!(r.text_body.contains("by Acme Inc"));
        assert!(r.text_body.contains(decline));
        assert!(r.html_body.contains("Acme Inc"));
        assert!(r.html_body.contains("Block this address"));
    }

    #[test]
    fn renders_without_attribution() {
        let r = render(
            "Uptimepage",
            "Ops",
            "https://app/verify?token=t",
            24,
            None,
            None,
        );
        assert!(r.text_body.contains("alert channel \"Ops\" on Uptimepage"));
        assert!(!r.text_body.contains(" by "));
    }
}
