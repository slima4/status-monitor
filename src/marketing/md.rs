//! Markdown event-stream helpers shared by the blog, legal, and docs
//! renderers. Sanitisation stays with each caller; nothing here touches
//! HTML safety.

use std::collections::HashMap;

use pulldown_cmark::{CowStr, Event, HeadingLevel, Tag, TagEnd};

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

/// One entry of a page's on-page table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub level: u8,
    pub id: String,
    pub text: String,
}

/// Give every `h2`/`h3` a stable id and return the matching contents list.
/// The id has to be derived from the heading's *text*, which only arrives
/// after the opening event, so the stream is buffered rather than mapped
/// lazily — one pass over an already-materialised document at boot. Ids
/// and the returned entries come from the same pass, so a contents link
/// can never point at an id the body does not carry.
pub fn anchor_headings(mut events: Vec<Event<'_>>) -> (Vec<Event<'_>>, Vec<TocEntry>) {
    let mut toc: Vec<TocEntry> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for i in 0..events.len() {
        let level = match &events[i] {
            Event::Start(Tag::Heading { level, .. }) => *level,
            _ => continue,
        };
        let level = match level {
            HeadingLevel::H2 => 2,
            HeadingLevel::H3 => 3,
            _ => continue,
        };
        let text = heading_text(&events[i + 1..]);
        if text.is_empty() {
            continue;
        }
        let id = unique_slug(&text, &mut seen);
        if let Event::Start(Tag::Heading { id: slot, .. }) = &mut events[i] {
            *slot = Some(CowStr::from(id.clone()));
        }
        toc.push(TocEntry { level, id, text });
    }
    (events, toc)
}

/// Visible text of the heading the slice opens with, up to its close.
fn heading_text(rest: &[Event<'_>]) -> String {
    let mut text = String::new();
    for ev in rest {
        match ev {
            Event::End(TagEnd::Heading(_)) => break,
            Event::Text(t) | Event::Code(t) => text.push_str(t),
            _ => {}
        }
    }
    text.trim().to_string()
}

/// GitHub's heading-anchor rule: lowercase, punctuation dropped, spaces
/// hyphenated. Existing cross-page links were authored against it, so the
/// rule is load-bearing rather than cosmetic.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if ch == ' ' || ch == '-' || ch == '_' {
            out.push('-');
        }
    }
    out
}

fn unique_slug(text: &str, seen: &mut HashMap<String, usize>) -> String {
    let base = slugify(text);
    let n = seen.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 {
        base
    } else {
        format!("{base}-{}", *n - 1)
    }
}

/// Rewrite the repo-relative `foo.md` links docs are authored with into
/// site paths. `dir` is the linking page's directory under `docs/`, so a
/// `../api.md` from a subdirectory resolves the same way it does on disk.
pub fn rewrite_doc_links<'a>(
    events: impl Iterator<Item = Event<'a>>,
    dir: &'a str,
) -> impl Iterator<Item = Event<'a>> {
    events.map(move |ev| match ev {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: match doc_link(&dest_url, dir) {
                Some(rewritten) => CowStr::from(rewritten),
                None => dest_url,
            },
            title,
            id,
        }),
        other => other,
    })
}

/// `Some(site path)` for a link into another doc, `None` for anything to
/// leave alone (absolute URLs, mail, same-page anchors, asset paths).
pub fn doc_link(dest: &str, dir: &str) -> Option<String> {
    if dest.starts_with('#') || dest.starts_with('/') || dest.contains("://") {
        return None;
    }
    let (path, anchor) = match dest.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (dest, None),
    };
    let stem = path.strip_suffix(".md")?;
    let mut segments: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for seg in stem.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    let mut out = format!("/docs/{}", segments.join("/"));
    if let Some(a) = anchor {
        out.push('#');
        out.push_str(a);
    }
    Some(out)
}
