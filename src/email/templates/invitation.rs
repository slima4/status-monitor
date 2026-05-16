use chrono::{DateTime, Utc};

use crate::email::templates::{attr_escape, html_escape};
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    org_name: &str,
    inviter_display: &str,
    accept_url: &str,
    decline_url: &str,
    expires_at: DateTime<Utc>,
) -> RenderedEmail {
    let expires_human = expires_at.format("%Y-%m-%d %H:%M UTC").to_string();
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

    // Inline HTML; keeps the template self-contained, no askama dep for
    // transactional mail. Substitution is positional + escaped because every
    // input is operator/system-controlled (URLs are signed tokens, org name
    // comes from the orgs table CHECK constraint).
    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">You're invited to {org_esc}</h2>\n\
         <p>{inviter_esc} invited you to join <strong>{org_esc}</strong> on {site_esc}.</p>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{accept_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">Accept invitation</a>\n\
         </p>\n\
         <p style=\"font-size:0.9em;color:#555;\">Or <a href=\"{decline_attr}\">decline</a>. This link expires at <strong>{expires_esc}</strong>.</p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">If you weren't expecting this, you can ignore the message — no account is created.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        org_esc = html_escape(org_name),
        inviter_esc = html_escape(inviter_display),
        site_esc = html_escape(site_name),
        accept_attr = attr_escape(accept_url),
        decline_attr = attr_escape(decline_url),
        expires_esc = html_escape(&expires_human),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
