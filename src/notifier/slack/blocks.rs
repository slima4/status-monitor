//! The slice of Slack's Block Kit the templates build on, with Slack's own
//! limits applied at construction: a block over the cap is refused as
//! `invalid_blocks`, which loses the whole alert rather than a corner of it.

use serde::Serialize;

use crate::text::{single_line, truncate_chars};

const HEADER_MAX: usize = 150;
pub const SECTION_MAX: usize = 3000;
const FIELD_MAX: usize = 2000;
const MAX_FIELDS: usize = 10;

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Header {
        text: Text,
    },
    Section {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<Text>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        fields: Vec<Text>,
    },
    Context {
        elements: Vec<Text>,
    },
    Actions {
        elements: Vec<Element>,
    },
}

impl Block {
    pub fn header(text: &str) -> Self {
        Self::Header {
            text: Text::plain(text),
        }
    }

    pub fn section(markdown: String) -> Self {
        Self::Section {
            text: Some(Text::mrkdwn(markdown, SECTION_MAX)),
            fields: Vec::new(),
        }
    }

    /// Two-column layout, capped at the ten Slack accepts. `None` for an empty
    /// set, because a section with neither text nor fields is refused.
    pub fn fields(mut fields: Vec<Text>) -> Option<Self> {
        if fields.is_empty() {
            return None;
        }
        fields.truncate(MAX_FIELDS);
        Some(Self::Section { text: None, fields })
    }

    pub fn context(markdown: String) -> Self {
        Self::Context {
            elements: vec![Text::mrkdwn(markdown, SECTION_MAX)],
        }
    }

    pub fn link_button(label: &str, url: &str) -> Self {
        Self::Actions {
            elements: vec![Element::Button {
                text: Text::plain(label),
                url: url.to_string(),
            }],
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Text {
    PlainText { text: String, emoji: bool },
    Mrkdwn { text: String },
}

impl Text {
    fn plain(text: &str) -> Self {
        Self::PlainText {
            text: truncate_chars(&single_line(text), HEADER_MAX),
            emoji: true,
        }
    }

    fn mrkdwn(text: String, max: usize) -> Self {
        Self::Mrkdwn {
            text: truncate_chars(&text, max),
        }
    }

    pub fn field(label: &str, value: &str) -> Self {
        Self::mrkdwn(format!("*{label}*\n{value}"), FIELD_MAX)
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Element {
    Button { text: Text, url: String },
}

/// Escape the three characters Slack mrkdwn treats specially, so customer text
/// renders literally rather than as a live link or a mention.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Angle brackets carry every mrkdwn control sequence, and `plain_text` shows
/// entities verbatim, so a header cannot use [`escape`]: it drops them instead.
pub fn plain_label(s: &str) -> String {
    s.replace(['<', '>'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_serialize_to_slacks_wire_names() {
        let v = serde_json::to_value(vec![
            Block::header("api"),
            Block::fields(vec![Text::field("Started", "now")]).unwrap(),
            Block::link_button("View incident", "https://example.test/i/1"),
        ])
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!([
                {"type": "header", "text": {"type": "plain_text", "text": "api", "emoji": true}},
                {"type": "section", "fields": [{"type": "mrkdwn", "text": "*Started*\nnow"}]},
                {"type": "actions", "elements": [{
                    "type": "button",
                    "text": {"type": "plain_text", "text": "View incident", "emoji": true},
                    "url": "https://example.test/i/1"
                }]}
            ])
        );
    }

    #[test]
    fn an_over_long_section_is_cut_to_slacks_cap() {
        let v = serde_json::to_value(Block::section("x".repeat(9_000))).unwrap();
        let text = v["text"]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), SECTION_MAX);
        assert!(text.ends_with('…'));
    }

    /// `plain_text` shows entities verbatim, so escaping here would put a
    /// literal `&amp;` in front of the responders.
    #[test]
    fn a_header_keeps_an_ampersand_as_typed() {
        let v = serde_json::to_value(Block::header(&plain_label("search & index"))).unwrap();
        assert_eq!(v["text"]["text"], "search & index");
    }

    #[test]
    fn a_header_cannot_carry_slack_markup() {
        let v = serde_json::to_value(Block::header(&plain_label("<!channel> down"))).unwrap();
        assert_eq!(v["text"]["text"], "!channel down");
    }
}
