use crate::email::templates::html_escape;
use crate::email::templates::layout::{self, ButtonStyle, Page};
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

    let mut body = layout::paragraph(&format!(
        "You asked to receive status updates for <strong>{page}</strong>. \
         Confirm this address and updates start arriving here.",
        page = html_escape(page_name),
    ));
    body.push_str(&layout::button(
        confirm_url,
        "Confirm subscription",
        ButtonStyle::Solid,
    ));
    body.push_str(&layout::fine_print(&format!(
        "This link expires in <strong>{expires_hours}</strong> hours and can only be used once."
    )));

    let footnote = layout::fine_print(&format!(
        "Didn't request this? {remove} and nothing will be sent to it.",
        remove = layout::quiet_link(unsubscribe_url, "Remove this address"),
    ));

    let html_body = layout::render(Page {
        title: &subject,
        preheader: "One click and status updates start arriving here.",
        signature: Some(site_name),
        header: layout::wordmark(site_name, "Confirm your subscription"),
        body,
        footnote: Some(footnote),
    });

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
    fn both_the_confirm_and_the_opt_out_survive_rendering() {
        let r = render(
            "Uptimepage",
            "Acme status",
            "https://acme.test/subscribe/confirm?token=x",
            24,
            "https://acme.test/subscribe/unsubscribe?s=1&t=2",
        );
        assert!(r.subject.contains("Acme status"));
        assert!(r.html_body.contains("subscribe/confirm?token=x"));
        assert!(r.html_body.contains("Remove this address"));
        assert!(r.text_body.contains("subscribe/unsubscribe?s=1&t=2"));
    }
}
