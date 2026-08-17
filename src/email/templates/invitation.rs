use chrono::{DateTime, Utc};

use crate::email::templates::layout::{self, ButtonStyle, Page};
use crate::email::templates::{html_escape, utc_stamp};
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    org_name: &str,
    inviter_display: &str,
    accept_url: &str,
    decline_url: &str,
    expires_at: DateTime<Utc>,
) -> RenderedEmail {
    let expires_human = utc_stamp(expires_at);
    let subject = format!("{inviter_display} invited you to {org_name}");

    let text_body = format!(
        "{inviter_display} invited you to join {org_name} on {site_name}.\n\
         \n\
         Accept the invitation:\n  {accept_url}\n\
         \n\
         Or decline:\n  {decline_url}\n\
         \n\
         This link expires at {expires_human}.\n\
         \n\
         If you weren't expecting this, you can ignore the message — no\n\
         account is created and the invitation will be cleaned up after it\n\
         expires.\n"
    );

    let mut body = layout::paragraph(&format!(
        "{inviter} invited you to join <strong>{org}</strong> on {site}.",
        inviter = html_escape(inviter_display),
        org = html_escape(org_name),
        site = html_escape(site_name),
    ));
    body.push_str(&layout::button(
        accept_url,
        "Accept invitation",
        ButtonStyle::Solid,
    ));
    body.push_str(&layout::fine_print(&format!(
        "Or {decline}. This link expires at <strong>{expires}</strong>.",
        decline = layout::link(decline_url, "decline"),
        expires = html_escape(&expires_human),
    )));

    let html_body = layout::render(Page {
        title: &subject,
        preheader: &format!("Join {org_name} on {site_name}."),
        signature: Some(site_name),
        header: layout::wordmark(site_name, &format!("You're invited to {org_name}")),
        body,
        footnote: Some(layout::fine_print(
            "If you weren't expecting this, you can ignore the message — no account is created.",
        )),
    });

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
