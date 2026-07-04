use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

/// Body's first line is the subject. Footer attributes the sending org and
/// carries the recipient's one-click stop link.
pub fn render(
    site_name: &str,
    body: &str,
    org_name: Option<&str>,
    stop_url: Option<&str>,
) -> RenderedEmail {
    let subject = body.lines().next().unwrap_or("incident alert").to_string();

    let mut attribution = String::new();
    if let Some(org) = org_name {
        attribution.push_str(&format!(
            "\nYou're receiving this because {org} added this address as an alert channel on {site_name}."
        ));
    }
    if let Some(url) = stop_url {
        attribution.push_str(&format!("\nStop delivery to this address: {url}"));
    }
    let text_body = format!("{body}\n{attribution}\n\n— {site_name} alerts\n");

    let attribution_html = {
        let mut html = String::new();
        if let Some(org) = org_name {
            html.push_str(&format!(
                "You're receiving this because <strong>{org}</strong> added this \
                 address as an alert channel on {site}.",
                org = html_escape(org),
                site = html_escape(site_name),
            ));
        }
        if let Some(url) = stop_url {
            html.push_str(&format!(
                " <a href=\"{url_attr}\">Stop delivery to this address</a>.",
                url_attr = html_escape(url),
            ));
        }
        html
    };

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <pre style=\"font-family:ui-monospace,monospace;white-space:pre-wrap;\">{body_esc}</pre>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">{attribution_html}</p>\n\
         <p style=\"font-size:0.8em;color:#888;\">— {site_esc} alerts</p>\n\
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

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn attribution_and_stop_link_appear_when_present() {
        let stop = "https://app/alert-channel/stop?c=1&t=2";
        let r = render(
            "Uptimepage",
            "api — major incident OPEN",
            Some("Acme Inc"),
            Some(stop),
        );
        assert_eq!(r.subject, "api — major incident OPEN");
        assert!(r.text_body.contains("Acme Inc"));
        assert!(r.text_body.contains(stop));
        assert!(r.html_body.contains("Acme Inc"));
    }

    #[test]
    fn no_footer_lines_without_context() {
        let r = render("Uptimepage", "body", None, None);
        assert!(!r.text_body.contains("You're receiving"));
        assert!(!r.text_body.contains("Stop delivery"));
    }
}
