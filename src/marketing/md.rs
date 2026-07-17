//! Markdown event-stream helpers shared by the blog and legal renderers.
//! Sanitisation stays with each caller; nothing here touches HTML safety.

use pulldown_cmark::{Event, Tag, TagEnd};

/// CommonMark emits bare `<table>` with no class hook; the scroll container
/// keeps a wide table from panning the whole page sideways on mobile.
/// `tabindex` makes the scroll region keyboard-reachable where the browser
/// does not focus scrollable boxes on its own (Safari).
pub fn wrap_tables<'a>(events: impl Iterator<Item = Event<'a>>) -> impl Iterator<Item = Event<'a>> {
    events
        .flat_map(|ev| match ev {
            Event::Start(Tag::Table(_)) => [
                Some(Event::Html(
                    "<div class=\"mk-table-scroll\" tabindex=\"0\">\n".into(),
                )),
                Some(ev),
            ],
            Event::End(TagEnd::Table) => [Some(ev), Some(Event::Html("</div>".into()))],
            _ => [Some(ev), None],
        })
        .flatten()
}
