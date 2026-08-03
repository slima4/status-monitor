//! Account-restored notification. The deletion mail promised a date; this one
//! retracts it.
//!
//! Inline HTML, same self-contained shape as the sibling templates.

use crate::email::templates::html_escape;
use crate::email::trait_def::RenderedEmail;

pub fn render(site_name: &str) -> RenderedEmail {
    let subject = format!("Your {site_name} account has been restored");

    let text_body = format!(
        "The deletion of your {site_name} account has been cancelled.\n\
         \n\
         Your account, your organisations, and their monitors and status pages\n\
         are active again, and monitoring has resumed.\n\
         \n\
         If this was not you, sign in and delete the account again, then get in\n\
         touch — someone else can reach your sign-in method.\n"
    );

    let html_body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>{subject_esc}</title></head>\n\
         <body style=\"font-family:system-ui,sans-serif;max-width:560px;margin:2rem auto;color:#222;\">\n\
         <h2 style=\"margin-top:0;\">Account restored</h2>\n\
         <p>The deletion of your {site_esc} account has been cancelled.</p>\n\
         <p>Your account, your organisations, and their monitors and status pages are active again, and monitoring has resumed.</p>\n\
         <p style=\"font-size:0.9em;color:#555;border-top:1px solid #eee;padding-top:1rem;\">If this was not you, sign in and delete the account again, then get in touch — someone else can reach your sign-in method.</p>\n\
         </body></html>\n",
        subject_esc = html_escape(&subject),
        site_esc = html_escape(site_name),
    );

    RenderedEmail {
        subject,
        text_body,
        html_body,
    }
}
