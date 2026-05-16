//! Account-deletion confirmation email. Sent once, right after a successful
//! `DELETE /api/v1/me`. Carries the single-use recovery link and the date the
//! account and all its data are permanently erased.
//!
//! Inline HTML, positional + escaped substitution — same self-contained shape
//! as the invitation / magic-link templates (no askama dep for transactional
//! mail). The only interpolated free-text is the recovery URL (a signed
//! token) and a formatted date.

use chrono::{DateTime, Utc};

use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

pub fn render(
    site_name: &str,
    recovery_url: &str,
    scheduled_purge_at: DateTime<Utc>,
) -> RenderedEmail {
    let purge_human = scheduled_purge_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let subject = format!("Your {site_name} account is scheduled for deletion");

    let text_body = format!(
        "Your {site_name} account has been deactivated and is scheduled for\n\
         permanent deletion on {purge_human}.\n\
         \n\
         Changed your mind? Restore your account and any organisations you\n\
         solely own with this one-time link:\n\
         \n  {recovery_url}\n\
         \n\
         The link works until {purge_human}. After that the account and all\n\
         associated data are permanently erased and cannot be recovered.\n\
         \n\
         If you requested this deletion, no further action is needed.\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Account scheduled for deletion</h2>\n\
         <p>Your {site_esc} account has been deactivated and is scheduled for permanent deletion on <strong>{purge_esc}</strong>.</p>\n\
         <p style=\"margin:1.5rem 0;\">\n\
           <a href=\"{recover_attr}\" style=\"background:#0b66e4;color:#fff;padding:10px 16px;border-radius:6px;text-decoration:none;\">Restore my account</a>\n\
         </p>\n\
         <p style=\"font-size:0.9em;color:#555;\">This one-time link works until <strong>{purge_esc}</strong>. After that the account and all associated data are permanently erased and cannot be recovered.</p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">If you requested this deletion, no further action is needed.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        site_esc = html_escape(site_name),
        purge_esc = html_escape(&purge_human),
        recover_attr = html_escape(recovery_url),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
