use crate::domain::IncidentStatusPhase;
use crate::email::templates::layout::{self, ButtonStyle, Page, Tone};
use crate::email::templates::{html_escape, single_line};
use crate::email::trait_def::RenderedEmail;

/// Capitalised phase label ("investigating" -> "Investigating").
fn phase_label(phase: &str) -> String {
    let mut chars = phase.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn render(
    page_name: &str,
    incident_title: &str,
    phase: &str,
    message: &str,
    incident_url: &str,
    unsubscribe_url: &str,
) -> RenderedEmail {
    let label = phase_label(phase);
    let subject = format!("[{page_name}] {incident_title} — {label}");

    let text_body = format!(
        "{incident_title}\n\
         Status: {label}\n\
         \n\
         {message}\n\
         \n\
         View the status page:\n  {incident_url}\n\
         \n\
         Unsubscribe:\n  {unsubscribe_url}\n"
    );

    // Parsed back to the enum so a new phase has to be classified here rather
    // than falling into "still going wrong" by default.
    let tone = match IncidentStatusPhase::from_db_str(phase) {
        IncidentStatusPhase::Resolved | IncidentStatusPhase::Postmortem => Tone::Good,
        IncidentStatusPhase::Investigating
        | IncidentStatusPhase::Identified
        | IncidentStatusPhase::Monitoring => Tone::Warn,
    };

    let mut body = layout::prose(message);
    body.push_str(&layout::button(
        incident_url,
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
        preheader: &single_line(message),
        site_name: page_name,
        header: layout::band(tone, &label.to_uppercase(), incident_title, None),
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
    fn phase_drives_the_status_band_and_the_subject() {
        let r = render(
            "Acme status",
            "Checkout is failing",
            "investigating",
            "We are looking into elevated errors.",
            "https://acme.test/incidents/1",
            "https://acme.test/subscribe/unsubscribe?s=1&t=2",
        );
        assert_eq!(
            r.subject,
            "[Acme status] Checkout is failing — Investigating"
        );
        assert!(r.html_body.contains("INVESTIGATING"));
        assert!(r.html_body.contains("We are looking into elevated errors."));
        assert!(r.html_body.contains("Unsubscribe"));
    }

    #[test]
    fn a_postmortem_does_not_read_as_an_open_problem() {
        let r = render(
            "Acme status",
            "Checkout is failing",
            "postmortem",
            "What happened and what we changed.",
            "https://acme.test/incidents/1",
            "https://acme.test/subscribe/unsubscribe?s=1&t=2",
        );
        // Tone::Good's signal colour; the warn amber would say "still broken".
        assert!(r.html_body.contains("#43d58f"), "{}", r.html_body);
        assert!(!r.html_body.contains("#f3b94c"));
    }

    #[test]
    fn an_update_written_in_paragraphs_keeps_its_line_breaks() {
        let r = render(
            "Acme status",
            "Checkout is failing",
            "resolved",
            "Root cause found.\nA fix is deployed.",
            "https://acme.test/incidents/1",
            "https://acme.test/subscribe/unsubscribe?s=1&t=2",
        );
        assert!(
            r.html_body
                .contains("Root cause found.<br>A fix is deployed.")
        );
    }
}
