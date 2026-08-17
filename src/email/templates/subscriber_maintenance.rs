use chrono::{DateTime, Utc};

use crate::email::templates::layout::{self, ButtonStyle, Page, Tone};
use crate::email::templates::{html_escape, utc_stamp};
use crate::email::trait_def::RenderedEmail;

#[allow(clippy::too_many_arguments)]
pub fn render(
    page_name: &str,
    title: &str,
    description: Option<&str>,
    phase: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    page_url: &str,
    unsubscribe_url: &str,
) -> RenderedEmail {
    let completed = phase == "completed";
    let heading = if completed {
        "Maintenance completed"
    } else {
        "Scheduled maintenance"
    };
    let subject = format!("[{page_name}] {heading}: {title}");
    let window = format!("{} — {}", utc_stamp(starts_at), utc_stamp(ends_at));
    let desc_text = description.map(|d| format!("\n{d}\n")).unwrap_or_default();

    let text_body = format!(
        "{title}\n\
         {heading}\n\
         When: {window}\n\
         {desc_text}\n\
         View the status page:\n  {page_url}\n\
         \n\
         Unsubscribe:\n  {unsubscribe_url}\n"
    );

    let mut body = layout::facts(&[(if completed { "Ran" } else { "Window" }, window.clone())]);
    if let Some(description) = description {
        body.push_str(&layout::prose(description));
    }
    body.push_str(&layout::button(
        page_url,
        "View status page",
        ButtonStyle::Solid,
    ));

    let footnote = layout::fine_print(&format!(
        "You're receiving this because you subscribed to {page}. {unsub}.",
        page = html_escape(page_name),
        unsub = layout::quiet_link(unsubscribe_url, "Unsubscribe"),
    ));

    let html_body = layout::render(Page {
        title: &subject,
        preheader: &window,
        // A customer's subscribers hear from the page, not from us.
        signature: None,
        header: layout::band(
            if completed { Tone::Good } else { Tone::Info },
            &heading.to_uppercase(),
            title,
            Some(&window),
        ),
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
    use chrono::{TimeZone, Utc};

    fn rendered(phase: &str) -> crate::email::trait_def::RenderedEmail {
        render(
            "Acme status",
            "Database upgrade",
            Some("Writes pause for a few minutes."),
            phase,
            Utc.with_ymd_and_hms(2026, 8, 20, 1, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 20, 3, 0, 0).unwrap(),
            "https://acme.test/",
            "https://acme.test/subscribe/unsubscribe?s=1&t=2",
        )
    }

    #[test]
    fn the_window_reaches_both_bodies_in_utc() {
        let r = rendered("scheduled");
        for body in [&r.text_body, &r.html_body] {
            assert!(
                body.contains("20 Aug 2026 01:00 UTC — 20 Aug 2026 03:00 UTC"),
                "window: {body}"
            );
        }
        assert!(r.html_body.contains("SCHEDULED MAINTENANCE"));
    }

    #[test]
    fn a_completed_window_is_reported_in_the_past() {
        let r = rendered("completed");
        assert!(r.subject.contains("Maintenance completed"));
        assert!(r.html_body.contains("MAINTENANCE COMPLETED"));
        assert!(r.html_body.contains("RAN"), "fact labels ship upper-cased");
    }
}
