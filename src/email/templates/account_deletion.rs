//! Account-deletion notification email. Sent once, right after a successful
//! `DELETE /api/v1/me`. Tells the user the account is deactivated and when it
//! is permanently erased. No link to carry: restoring runs through a
//! signed-in confirmation, not this mail.
//!
//! Inline HTML, positional + escaped substitution — same self-contained shape
//! as the invitation / magic-link templates (no askama dep for transactional
//! mail). The only interpolated free-text is a formatted date.

use chrono::{DateTime, Utc};

use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

pub fn render(site_name: &str, scheduled_purge_at: DateTime<Utc>) -> RenderedEmail {
    let purge_human = scheduled_purge_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let subject = format!("Your {site_name} account is scheduled for deletion");

    let text_body = format!(
        "Your {site_name} account has been deactivated and is scheduled for\n\
         permanent deletion on {purge_human}. Monitoring has stopped: your\n\
         checks are no longer running and no alerts will be sent.\n\
         \n\
         Changed your mind? Sign in before {purge_human} and confirm the\n\
         restore on the page that appears; signing in on its own will not\n\
         cancel the deletion.\n\
         \n\
         After that the account and all associated data are permanently erased\n\
         and cannot be recovered.\n\
         \n\
         If you requested this deletion, no further action is needed.\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Account scheduled for deletion</h2>\n\
         <p>Your {site_esc} account has been deactivated and is scheduled for permanent deletion on <strong>{purge_esc}</strong>. Monitoring has stopped: your checks are no longer running and no alerts will be sent.</p>\n\
         <p style=\"margin:1.5rem 0;\">Changed your mind? <strong>Sign in before {purge_esc}</strong> and confirm the restore on the page that appears — signing in on its own will not cancel the deletion.</p>\n\
         <p style=\"font-size:0.9em;color:#555;\">After that the account and all associated data are permanently erased and cannot be recovered.</p>\n\
         <p style=\"font-size:0.8em;color:#888;border-top:1px solid #eee;padding-top:1rem;\">If you requested this deletion, no further action is needed.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        site_esc = html_escape(site_name),
        purge_esc = html_escape(&purge_human),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
