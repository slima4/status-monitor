//! Code-fence highlighting for the Markdown renderers.
//!
//! Tokens carry classes rather than an inline `style`, because the marketing
//! CSP is `style-src 'self'`. Colours live in `_marketing.css`.

use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Tag, TagEnd};
use syntect::html::{ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Prefixed so a scope atom can never collide with a Tailwind utility.
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

struct OpenBlock {
    lang: String,
    syntax: &'static SyntaxReference,
    code: String,
}

/// An unknown language passes through untouched, so the stylesheet's
/// plain-fence rules still key off the class pulldown would have written.
pub fn code_blocks<'a>(events: impl Iterator<Item = Event<'a>>) -> Vec<Event<'a>> {
    let mut out: Vec<Event<'a>> = Vec::new();
    let mut open: Option<OpenBlock> = None;
    for event in events {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info))) => {
                let opened = token(info).and_then(|lang| {
                    syntax(lang).map(|syntax| OpenBlock {
                        lang: lang.to_string(),
                        syntax,
                        code: String::new(),
                    })
                });
                match opened {
                    Some(block) => open = Some(block),
                    None => out.push(event),
                }
            }
            Event::End(TagEnd::CodeBlock) => match open.take() {
                Some(block) => out.push(Event::Html(CowStr::from(render(&block)))),
                None => out.push(event),
            },
            Event::Text(ref text) => match open.as_mut() {
                Some(block) => block.code.push_str(text),
                None => out.push(event),
            },
            other => out.push(other),
        }
    }
    out
}

/// Widen a sanitiser to keep what this module emits. Filtering on the class
/// value rather than the tag stops authored markup borrowing the opening:
/// hand-written `<code class="mk-code-copy">` in a post would otherwise take
/// a site hook, and `.mk-code-copy` is absolutely positioned.
pub fn allow_markup(builder: &mut ammonia::Builder<'_>) {
    builder
        .add_tag_attributes("span", &["class"])
        .add_tag_attributes("code", &["class"])
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("span", "class") => all_prefixed(value, "hl-").then(|| value.into()),
            ("code", "class") => all_prefixed(value, "language-").then(|| value.into()),
            _ => Some(value.into()),
        });
}

fn all_prefixed(value: &str, prefix: &str) -> bool {
    let mut classes = value.split_whitespace().peekable();
    classes.peek().is_some() && classes.all(|class| class.starts_with(prefix))
}

/// A fence's info string may carry attributes: ```` ```rust,no_run ````.
fn token(info: &str) -> Option<&str> {
    let word = info.split([' ', ',', ';']).next()?.trim();
    (!word.is_empty()).then_some(word)
}

/// Fence words no syntax answers to. `jsonc` has no definition anywhere;
/// JavaScript reads comment-bearing JSON without complaint.
fn alias(word: &str) -> &str {
    match word {
        "jsonc" | "json5" => "js",
        "hcl" | "terraform" => "tf",
        "shell" | "console" => "bash",
        other => other,
    }
}

fn syntax(word: &str) -> Option<&'static SyntaxReference> {
    let word = alias(word);
    SYNTAXES
        .find_syntax_by_extension(word)
        .or_else(|| SYNTAXES.find_syntax_by_token(word))
        .or_else(|| {
            SYNTAXES
                .syntaxes()
                .iter()
                .find(|syntax| syntax.name.eq_ignore_ascii_case(word))
        })
}

fn render(block: &OpenBlock) -> String {
    let mut generator =
        ClassedHTMLGenerator::new_with_class_style(block.syntax, &SYNTAXES, CLASS_STYLE);
    for line in LinesWithEndings::from(&block.code) {
        // Half a coloured block is worse than none; the code still reads.
        if generator
            .parse_html_for_line_which_includes_newline(line)
            .is_err()
        {
            return fence(&block.lang, &ammonia::clean_text(&block.code));
        }
    }
    fence(&block.lang, &generator.finalize())
}

/// Mirrors pulldown's own fence markup, so the stylesheet needs no second
/// selector for the blocks this module writes.
fn fence(lang: &str, body: &str) -> String {
    format!(
        "<pre><code class=\"language-{}\">{body}</code></pre>",
        ammonia::clean_text(lang)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(markdown: &str) -> String {
        let mut out = String::new();
        let events = code_blocks(pulldown_cmark::Parser::new(markdown));
        pulldown_cmark::html::push_html(&mut out, events.into_iter());
        out
    }

    fn text_of(html: &str) -> String {
        let mut out = String::new();
        let mut inside = false;
        for ch in html.chars() {
            match ch {
                '<' => inside = true,
                '>' => inside = false,
                c if !inside => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn every_language_used_in_content_resolves() {
        for word in [
            "bash",
            "jsonc",
            "rust",
            "toml",
            "sql",
            "yaml",
            "terraform",
            "xml",
            "markdown",
            "json",
            "js",
            "http",
            "hcl",
            "go",
        ] {
            assert!(syntax(word).is_some(), "no syntax for `{word}`");
        }
    }

    /// A `text` fence is literal output: a log line, a tree, a paste.
    #[test]
    fn text_fence_is_left_alone() {
        let out = html("```text\nGET / 200\n```");
        assert!(
            out.contains("<code class=\"language-text\">GET / 200"),
            "{out}"
        );
    }

    #[test]
    fn tokens_become_prefixed_classes() {
        let out = html("```rust\nlet x = 1;\n```");
        assert!(out.contains("<pre><code class=\"language-rust\">"), "{out}");
        assert!(out.contains("hl-keyword"), "{out}");
        assert!(!out.contains("style="), "{out}");
    }

    #[test]
    fn unknown_language_keeps_its_plain_block() {
        let out = html("```nosuchlang\nhello\n```");
        assert!(
            out.contains("<code class=\"language-nosuchlang\">hello"),
            "{out}"
        );
    }

    #[test]
    fn plain_fence_keeps_its_missing_class() {
        let out = html("```\n$ curl -s localhost\n```");
        assert!(out.contains("<pre><code>$ curl"), "{out}");
    }

    #[test]
    fn code_text_survives_intact() {
        let out = html("```sql\nSELECT 1 < 2 AS \"ok\";\n```");
        let text = text_of(&out).replace("&lt;", "<").replace("&quot;", "\"");
        assert!(text.contains("SELECT 1 < 2 AS \"ok\";"), "{text}");
    }

    #[test]
    fn only_prefixed_class_lists_pass_the_filter() {
        assert!(all_prefixed("hl-source hl-rust", "hl-"));
        assert!(!all_prefixed("", "hl-"));
        assert!(!all_prefixed("hl-source mk-fig", "hl-"));
    }

    /// A post is third-party input, and `<span>`/`<code>` are both in
    /// ammonia's default tag set, so authored markup reaches the filter.
    #[test]
    fn authored_classes_do_not_survive() {
        let mut safe = ammonia::Builder::default();
        allow_markup(&mut safe);
        let cleaned = safe.clean(
            "<code class=\"mk-code-copy\">a</code><span class=\"mk-fig\">b</span>\
             <code class=\"language-rust\">c</code><span class=\"hl-keyword\">d</span>",
        );
        assert_eq!(
            cleaned.to_string(),
            "<code>a</code><span>b</span>\
             <code class=\"language-rust\">c</code><span class=\"hl-keyword\">d</span>"
        );
    }
}
