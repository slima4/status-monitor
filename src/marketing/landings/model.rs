use crate::marketing::config::BRAND;

/// A use-case or comparison landing page. Comparison and use-case pages
/// share one shape — the only difference is copy — so there is no kind
/// discriminant to keep in lockstep.
pub struct Landing {
    pub path: &'static str,
    pub created: &'static str,
    pub lastmod: &'static str,
    /// `<title>` and OpenGraph title (brand suffix added at render).
    pub title: &'static str,
    pub eyebrow: &'static str,
    pub h1: &'static str,
    pub meta_description: &'static str,
    pub lede: &'static str,
    pub features: &'static [Feature],
    pub sections: &'static [Section],
    pub code: Option<CodeSample>,
    pub resources: &'static [ResourceLink],
    pub cta: &'static str,
}

/// One row in a landing page's "what you get" table.
pub struct Feature {
    pub label: &'static str,
    pub value: &'static str,
}

/// One prose block in the page body.
pub struct Section {
    pub heading: &'static str,
    pub body: &'static str,
}

/// A replayable figure a landing can mount: the class its script looks for,
/// the script that fills it, and the prose around it. One struct so the mount
/// and the script it needs cannot drift apart.
pub(super) struct Figure {
    pub mount: &'static str,
    pub script: &'static str,
    pub heading: &'static str,
    pub caption: &'static str,
}

pub struct ResourceLink {
    pub label: &'static str,
    pub href: &'static str,
}

pub struct CodeSample {
    pub caption: &'static str,
    pub body: &'static str,
}

/// One row of a head-to-head comparison matrix: a label plus one
/// `(text, tone)` cell per column. `tone` is a `.cmp` cell class
/// (`""`, `"yes"`, `"no"`, `"part"`) that colours the value.
pub(in crate::marketing) struct MatrixRow {
    pub label: &'static str,
    pub cells: &'static [(&'static str, &'static str)],
}

/// A factual, dated comparison matrix. Only the head-to-head page carries
/// one, so it is looked up by path (like the FAQs) rather than stored on
/// every `Landing`. Keep `notes` verifiable and the last one dated.
pub(in crate::marketing) struct Matrix {
    pub heading: &'static str,
    pub columns: &'static [&'static str],
    pub rows: &'static [MatrixRow],
    pub notes: &'static [&'static str],
}

impl Matrix {
    /// Index of the highlighted Uptimepage column, wherever it sits: first
    /// on `/vs/` pages, last on `/compare/` face-offs.
    pub fn us_col(&self) -> usize {
        self.columns
            .iter()
            .position(|c| *c == BRAND)
            .expect("matrix missing Uptimepage column")
    }
}

/// One of the three "which of these is for you" cards above the table. The
/// last card is always ours, so the template highlights by position rather
/// than a flag every card has to carry.
pub(super) struct PickCard {
    pub label: &'static str,
    pub body: &'static str,
}

/// One pane of the hero config viewer. `lines` is authored one line per
/// entry so the gutter can number them, and each carries `mk-conf__*` token
/// spans rather than going through the Markdown highlighter, whose Nord
/// palette is scoped to article and doc bodies.
pub(super) struct ConfigPane {
    pub id: &'static str,
    pub tab: &'static str,
    pub cmd: &'static str,
    pub tag: &'static str,
    pub note: &'static str,
    pub lines: &'static [&'static str],
}

/// One monitor in the status-page mock beside the closing pitch. `days` is
/// one character per day, oldest first: `u` up, `d` degraded, `x` down.
pub(super) struct MockRow {
    pub name: &'static str,
    pub uptime: &'static str,
    pub note: &'static str,
    pub days: &'static str,
}

impl MockRow {
    /// A class per day, so the template prints the bar without carrying
    /// forty-five spans of markup per row.
    pub fn cells(&self) -> Vec<&'static str> {
        self.days
            .chars()
            .map(|day| match day {
                'x' => "mk-mock__day mk-mock__day--down",
                'd' => "mk-mock__day mk-mock__day--warn",
                _ => "mk-mock__day",
            })
            .collect()
    }
}

/// `/vs/` and `/compare/` share the head-to-head layout; use-case landings
/// keep the plain stacked one.
pub(super) fn is_comparison(path: &str) -> bool {
    path.starts_with("/vs/") || path.starts_with("/compare/")
}
