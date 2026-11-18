//! Post and comment bodies: markdown drawn as Dioxus elements.
//!
//! pulldown-cmark's events are folded into a small tree (`Node`) and each
//! node is drawn by `node`, one `rsx!` per element. Nothing writes HTML,
//! so there is nothing to sanitise afterwards: every text and attribute
//! value is set as a value, and dioxus-ssr escapes it. What agora-server's
//! sanitiser decides (`web::markdown::render`: pulldown-cmark, then a
//! pinned ammonia) is decided here instead, while the tree is built:
//!
//! - Raw HTML, inline or block, is text.
//! - A link's or image's destination becomes an `href` only if it is
//!   relative or its scheme is in [`LINK_SCHEMES`]; otherwise the link is
//!   an `<a>` with no `href` (the server's sanitiser drops the attribute
//!   and keeps the element). It is percent-encoded first, as the server's
//!   writer does, and checked as encoded, so the check reads what the
//!   browser will. An `agora://` link becomes its page's path
//!   ([`AgoraLink::site_path`]); one that doesn't parse is an unknown
//!   scheme.
//! - An image is a link to it, its alt text the link's text: nothing in a
//!   body makes the reader's browser fetch anything.
//! - Every `<a>` has a `rel`: [`SITE_LINK_REL`] on a link to Agora's own
//!   pages ([`is_on_site`]), [`LINK_REL`] (`nofollow ugc` too) on any
//!   other. Besides `rel` it has only `href` and `title`.
//! - A code block's `class` is `language-<name>` only for a name in
//!   [`LANGUAGES`], matched against the info string's first word and
//!   taken from the table, never from the input. No other element has a
//!   `class`, and none has a `style` or `id`.
//! - NUL is dropped from text (as a browser's parser drops it) and is
//!   U+FFFD in a title (as it reads an attribute's).
//! - Text the parser puts where a table's or list's parts go (it does,
//!   after a table that ran out of padding) is drawn after the container.
//!
//! What keeps the server's tree the browser's (hydration adopts nodes by
//! position, so the HTML parser must not restructure what was drawn):
//!
//! - No link inside a link: an image inside a link is drawn as its alt
//!   text, in the outer link.
//! - Nesting stops at [`MAX_DEPTH`] elements: deeper elements are left
//!   out and their contents kept, in place. A table or list is left out
//!   whole when its rows or items wouldn't fit, never cut in the middle.
//! - A text longer than [`TEXT_CHUNK`] bytes is drawn as several text
//!   nodes: browsers split a text node past 65,536 UTF-16 units, and
//!   hydration would lose the second half.
//!
//! And what bounds the work and the page by the source's length, so a
//! body is never a multiplier:
//!
//! - Tables are padded as pulldown-cmark pads them (every row to its
//!   header's width), as the server draws them, up to [`TABLE_CELLS`]
//!   cells in a body. Past that, the table that passes it and every later
//!   one are drawn without their rows' trailing empty cells (a 4 KiB table
//!   of 682 columns padded every row to 682 cells). Ragged rows are valid
//!   HTML and hydrate.
//! - At most one element per byte of source, plus [`ELEMENT_ALLOWANCE`].
//!   Every element a writer writes costs at least a byte and padding is
//!   never charged, so no input the review tried makes this bind: it is a
//!   backstop (tested with a small budget). Where it does bind: a row,
//!   cell or item is left out and its text drawn after its table or list;
//!   any other element is left out with its text kept in place. No text
//!   is dropped.
//! - The bytes of `href`s and titles are bounded by three times the
//!   source's length (percent-encoding triples a byte), so a reference
//!   link used many times can't multiply the page past it. Past the bound
//!   a link loses its `href` and title. pulldown-cmark bounds reference
//!   expansion too, more loosely (`max(source, 100 KB)` bytes).
//! - The builder does constant work per event.
//!
//! The parser's options are the server's (`options`): tables and
//! strikethrough. Footnotes, task lists, heading ids, math and smart
//! punctuation are off, so `[^1]`, `[ ]` and `{#id}` are text, as there.

use dioxus::prelude::*;
use pulldown_cmark::{
    CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag,
};

use url::Url;

use crate::links::AgoraLink;

/// The schemes a link may have, agora-server's sanitiser's (Steward,
/// 2026-10-23). A destination without one (a path, a `#fragment`) is
/// relative, and kept
pub const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// The `rel` of a link off Agora: it passes no ranking and no referrer
pub const LINK_REL: &str = "noopener noreferrer nofollow ugc";

/// The `rel` of a link to one of Agora's own pages ([`is_on_site`])
pub const SITE_LINK_REL: &str = "noopener noreferrer";

/// The deepest a body's elements nest; deeper ones are left out and their
/// contents kept. Far below a browser's own limit (Chromium's parser stops
/// nesting at 512), with room for the page around the body, and shallow
/// enough that drawing the tree, which recurses once per level, fits a
/// small stack: 64 KiB in a release build (tested; wasm's default is
/// 1 MiB), and wasm's 1 MiB in a debug build
pub const MAX_DEPTH: usize = 32;

const _: () = assert!(
    MAX_DEPTH <= 64,
    "drawing recurses once per level: keep MAX_DEPTH small"
);

/// The elements a body may have beyond one per byte of its source
pub const ELEMENT_ALLOWANCE: usize = 64;

/// The most table cells a body is drawn with as pulldown-cmark pads them,
/// every row to its header's width, as the server draws them. Past it,
/// counting the padding, the table that passes it and every later one are
/// drawn without their rows' trailing empty cells (each table one way
/// throughout: the rows already built lose them too). Padding costs no
/// source bytes and an empty cell's markup is about 55 bytes with its
/// hydration markers, so this bounds what padding adds to a body at about
/// 225 KB: a body of 64 × 64 tables of one-cell rows drew 2.7 MB from
/// 4 KiB (44 MB from 64 KiB) with a cap per table, and draws 345 KB (2.0 MB)
/// with this one, where the server draws 414 KB (6.4 MB). Honest tables up
/// to 4,096 cells in a body (12 columns × 300 rows) are drawn as the
/// server draws them
pub const TABLE_CELLS: usize = 4096;

/// The longest text drawn as one text node, in bytes: 16 KiB, so even
/// all-ASCII it is a quarter of a browser's 65,536 UTF-16 unit split
pub const TEXT_CHUNK: usize = 16 * 1024;

/// The code block languages that get a class, and the class each gets.
/// The info string's first word is matched against the names, ignoring
/// case; the class is this table's literal
pub const LANGUAGES: &[(&str, &str)] = &[
    ("bash", "language-bash"),
    ("c", "language-c"),
    ("console", "language-console"),
    ("cpp", "language-cpp"),
    ("css", "language-css"),
    ("csv", "language-csv"),
    ("diff", "language-diff"),
    ("dockerfile", "language-dockerfile"),
    ("go", "language-go"),
    ("html", "language-html"),
    ("http", "language-http"),
    ("ini", "language-ini"),
    ("java", "language-java"),
    ("javascript", "language-javascript"),
    ("js", "language-js"),
    ("json", "language-json"),
    ("makefile", "language-makefile"),
    ("markdown", "language-markdown"),
    ("md", "language-md"),
    ("python", "language-python"),
    ("py", "language-py"),
    ("rust", "language-rust"),
    ("sh", "language-sh"),
    ("shell", "language-shell"),
    ("sql", "language-sql"),
    ("text", "language-text"),
    ("toml", "language-toml"),
    ("ts", "language-ts"),
    ("typescript", "language-typescript"),
    ("xml", "language-xml"),
    ("yaml", "language-yaml"),
    ("yml", "language-yml"),
];

/// The parser's options, agora-server's: tables and strikethrough
pub(crate) fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// The website a body is drawn on, as context for [`Markdown`]: an
/// absolute link to one of its pages is the site's own ([`is_on_site`]).
/// Without it (or with `None`) only a relative link is. It comes from the
/// page's data ([`ThreadPage::site`](super::thread::ThreadPage::site)), so
/// the server's render and the client's agree
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Site(pub Option<Url>);

/// `source` as markdown, drawn as elements: a fragment of blocks, for the
/// caller's container (`div { class: "markdown-body", … }`). Its links'
/// `rel` reads the [`Site`] in context
#[component]
pub fn Markdown(source: String) -> Element {
    let site = try_use_context::<Site>().and_then(|site| site.0);
    let nodes = parse_at(&source, site.as_ref());
    rsx! {
        {nodes.iter().map(node)}
    }
}

/// `source` drawn as HTML with no hydration markers: a body for a page
/// that isn't hydrated (the server's own pages), the same elements as
/// [`Markdown`] on `site`
#[cfg(feature = "dioxus-ssr")]
pub fn to_html(source: &str, site: Option<&Url>) -> String {
    let mut dom = VirtualDom::new_with_props(
        Markdown,
        MarkdownProps {
            source: source.to_string(),
        },
    )
    .with_root_context(Site(site.cloned()));
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

/// One element of a body, or text
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Text(String),
    /// Inline code
    Code(String),
    Break,
    Rule,
    Element(Kind, Vec<Node>),
}

/// What an element is: the only elements a body draws
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Paragraph,
    Heading(HeadingLevel),
    Quote,
    /// Its class, from [`LANGUAGES`]
    CodeBlock(Option<&'static str>),
    /// `Some(start)` for a numbered list
    List(Option<u64>),
    Item,
    Table,
    /// The header row's cells (`thead > tr`)
    Head,
    /// A body row
    Row,
    Cell {
        head: bool,
    },
    Emphasis,
    Strong,
    Strikethrough,
    Link {
        href: Option<String>,
        title: Option<String>,
        rel: &'static str,
    },
}

impl Kind {
    /// How many levels the element needs below it to be whole: a table's
    /// rows and cells, a list's items
    fn levels(&self) -> usize {
        match self {
            Kind::Table => 3,
            Kind::List(_) => 2,
            _ => 1,
        }
    }

    /// Whether leaving it out should keep a line break where it was, so
    /// the words either side don't run together
    fn is_block(&self) -> bool {
        !matches!(
            self,
            Kind::Emphasis
                | Kind::Strong
                | Kind::Strikethrough
                | Kind::Link { .. }
        )
    }

    /// A part of a table or list, which can't be left out with its
    /// contents kept: they would be text directly in a `<tr>` or `<ul>`
    fn is_part(&self) -> bool {
        matches!(
            self,
            Kind::Head | Kind::Row | Kind::Cell { .. } | Kind::Item
        )
    }
}

/// A start tag, as its end will find it
#[derive(Debug, Clone, Copy)]
enum Frame {
    /// An element being filled: the top of [`Builder::open`]
    Open,
    /// Left out (too deep, over the budget, a link in a link, or not drawn
    /// at all); its contents go to the nearest open element. `block`: keep
    /// a line break; `deep`: counted in [`Builder::flattening`]
    LeftOut { block: bool, deep: bool },
    /// Left out with its contents: counted in [`Builder::skipping`]
    Skipped,
}

/// The tree's builder: explicit stacks, so a deep document can't overflow
/// the call stack, and constant work per event
struct Builder {
    root: Vec<Node>,
    /// One per start tag not yet ended
    frames: Vec<Frame>,
    /// The open elements, innermost last: at most [`MAX_DEPTH`]
    open: Vec<(Kind, Vec<Node>)>,
    /// How many open elements are links
    links: usize,
    /// While above zero, every start tag is left out (one was too deep or
    /// over the budget)
    flattening: usize,
    /// While above zero, everything is dropped
    skipping: usize,
    /// Elements still allowed
    elements: usize,
    /// Bytes of `href`s and titles still allowed
    link_bytes: usize,
    /// Text that had nowhere to go (in a part left out, or loose where a
    /// table's or list's parts go), kept to be drawn after the container
    spill: String,
    /// How many elements were open when `spill` started: it is drawn once
    /// the innermost of them closes
    spill_at: Option<usize>,
    /// Cells in the body's tables so far, padding included
    table_cells: usize,
    /// The tables passed [`TABLE_CELLS`]: the table being built loses its
    /// rows' trailing empty cells
    ragged: bool,
    /// The website the body is drawn on ([`Site`])
    site: Option<Url>,
}

/// How many empty cells end `cells`
fn trailing_empty(cells: &[Node]) -> usize {
    cells
        .iter()
        .rev()
        .take_while(|c| {
            matches!(c, Node::Element(Kind::Cell { .. }, inner) if inner.is_empty())
        })
        .count()
}

/// `source`'s tree, on no known site
#[cfg(all(test, feature = "dioxus-ssr"))]
pub(crate) fn parse(source: &str) -> Vec<Node> {
    parse_at(source, None)
}

/// `source`'s tree, on `site` ([`Site`])
pub(crate) fn parse_at(source: &str, site: Option<&Url>) -> Vec<Node> {
    let mut b = Builder::new(source.len(), site.cloned());
    for event in Parser::new_ext(source, options()) {
        b.event(event);
    }
    b.finish()
}

impl Builder {
    /// A builder for a source of `len` bytes, drawn on `site`
    fn new(len: usize, site: Option<Url>) -> Self {
        Self {
            site,
            root: Vec::new(),
            frames: Vec::new(),
            open: Vec::new(),
            links: 0,
            flattening: 0,
            skipping: 0,
            elements: len.saturating_add(ELEMENT_ALLOWANCE),
            link_bytes: len.saturating_mul(3),
            spill: String::new(),
            spill_at: None,
            table_cells: 0,
            ragged: false,
        }
    }

    /// The tree. The parser closes what it opens; anything left is closed
    /// here
    fn finish(mut self) -> Vec<Node> {
        while !self.frames.is_empty() {
            self.end();
        }
        let spill = std::mem::take(&mut self.spill);
        self.spill_at = None;
        self.text(&spill);
        self.root
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(_) => self.end(),
            // Raw HTML is shown as written, never interpreted
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                self.text(&t)
            }
            Event::Code(t) => self.push(Node::Code(t.replace('\0', ""))),
            Event::SoftBreak => self.text("\n"),
            Event::HardBreak => self.push(Node::Break),
            Event::Rule => self.push(Node::Rule),
            // Not produced under `options()`; as text, should they be
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                self.text("$");
                self.text(&t);
                self.text("$");
            }
            Event::FootnoteReference(name) => {
                self.text("[^");
                self.text(&name);
                self.text("]");
            }
            Event::TaskListMarker(done) => {
                self.text(if done { "[x] " } else { "[ ] " })
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        if self.skipping > 0 {
            self.skipping += 1;
            self.frames.push(Frame::Skipped);
            return;
        }
        let kind = match tag {
            Tag::Paragraph => Some(Kind::Paragraph),
            Tag::Heading { level, .. } => Some(Kind::Heading(level)),
            Tag::BlockQuote(_) => Some(Kind::Quote),
            Tag::CodeBlock(kind) => Some(Kind::CodeBlock(language(&kind))),
            Tag::List(start) => Some(Kind::List(start)),
            Tag::Item => Some(Kind::Item),
            Tag::Table(_) => Some(Kind::Table),
            Tag::TableHead => Some(Kind::Head),
            Tag::TableRow => Some(Kind::Row),
            Tag::TableCell => Some(Kind::Cell {
                head: matches!(self.open.last(), Some((Kind::Head, _))),
            }),
            Tag::Emphasis => Some(Kind::Emphasis),
            Tag::Strong => Some(Kind::Strong),
            Tag::Strikethrough => Some(Kind::Strikethrough),
            // An image is a link to it, its alt text the link's text
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            }
            | Tag::Image {
                link_type,
                dest_url,
                title,
                ..
            } => (self.links == 0 && self.flattening == 0)
                .then(|| self.link(link_type, dest_url, title)),
            // Its contents are text events; the rest are off under
            // `options()`
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => None,
        };
        let Some(kind) = kind else {
            self.frames.push(Frame::LeftOut {
                block: false,
                deep: false,
            });
            return;
        };
        // A table's or list's part left out can't keep its text in place
        // (it would be loose in a `<tr>` or `<ul>`): it is drawn after the
        // container. Nothing else goes where the parts go (pulldown-cmark
        // puts the lines after a table that ran out of padding in it)
        if self.flattening == 0
            && ((self.elements == 0 && kind.is_part()) || !self.fits(&kind))
        {
            self.skipping += 1;
            self.frames.push(Frame::Skipped);
            return;
        }
        if self.flattening > 0
            || self.elements < kind.levels()
            || self.open.len() + kind.levels() > MAX_DEPTH
        {
            self.flattening += 1;
            self.frames.push(Frame::LeftOut {
                block: kind.is_block(),
                deep: true,
            });
            return;
        }
        if matches!(kind, Kind::Link { .. }) {
            self.links += 1;
        }
        if kind == Kind::Table {
            // Once a body's tables have passed the cap, every later table
            // is drawn without its padding from its first row
            self.ragged = self.table_cells > TABLE_CELLS;
        }
        self.elements -= 1;
        self.frames.push(Frame::Open);
        self.open.push((kind, Vec::new()));
    }

    fn end(&mut self) {
        match self.frames.pop() {
            Some(Frame::Open) => {
                let (kind, children) =
                    self.open.pop().expect("an open frame has an element");
                if matches!(kind, Kind::Link { .. }) {
                    self.links -= 1;
                }
                let mut children = children;
                if matches!(kind, Kind::Head | Kind::Row) {
                    self.table_cells += children.len();
                }
                if kind == Kind::Row {
                    // A row's trailing empty cells (pulldown-cmark's
                    // padding to the header's width, or written ones) are
                    // given back: a writer's own cells are charged,
                    // padding never is. They are drawn, as the server
                    // draws them, unless the table passes TABLE_CELLS
                    let empty = trailing_empty(&children);
                    self.elements += empty;
                    if !self.ragged && self.table_cells > TABLE_CELLS {
                        // From here on the table is drawn without them,
                        // the rows already built included: one way
                        // throughout
                        self.ragged = true;
                        if let Some((Kind::Table, rows)) = self.open.last_mut()
                        {
                            for row in rows {
                                if let Node::Element(Kind::Row, cells) = row {
                                    let empty = trailing_empty(cells);
                                    cells.truncate(cells.len() - empty);
                                }
                            }
                        }
                    }
                    if self.ragged {
                        children.truncate(children.len() - empty);
                    }
                }
                // It was opened where it fits
                self.children().push(Node::Element(kind, children));
                // Text that had nowhere to go inside it follows it
                if self.spill_at.is_some_and(|at| self.open.len() < at) {
                    let spill = std::mem::take(&mut self.spill);
                    self.spill_at = None;
                    self.text(&spill);
                }
            }
            Some(Frame::LeftOut { block, deep }) => {
                if deep {
                    self.flattening -= 1;
                }
                if block {
                    self.line_break();
                }
            }
            Some(Frame::Skipped) => {
                self.skipping -= 1;
                // The words of two parts left out don't run together
                if !self.spill.is_empty()
                    && !self.spill.ends_with(char::is_whitespace)
                {
                    self.spill.push(' ');
                }
            }
            None => {}
        }
    }

    /// Whether `kind` may go in the innermost open element: only a table's
    /// head and rows in a table, cells in those, items in a list, and a
    /// part nowhere else
    fn fits(&self, kind: &Kind) -> bool {
        match self.open.last().map(|(k, _)| k) {
            Some(Kind::Table) => matches!(kind, Kind::Head | Kind::Row),
            Some(Kind::Head | Kind::Row) => matches!(kind, Kind::Cell { .. }),
            Some(Kind::List(_)) => matches!(kind, Kind::Item),
            _ => !kind.is_part(),
        }
    }

    /// Whether text may go in the innermost open element
    fn holds_text(&self) -> bool {
        !matches!(
            self.open.last(),
            Some((Kind::Table | Kind::Head | Kind::Row | Kind::List(_), _))
        )
    }

    /// Where a child goes: the innermost open element, or the root
    fn children(&mut self) -> &mut Vec<Node> {
        match self.open.last_mut() {
            Some((_, children)) => children,
            None => &mut self.root,
        }
    }

    fn push(&mut self, node: Node) {
        if self.skipping == 0 && self.holds_text() {
            self.children().push(node);
            return;
        }
        match node {
            Node::Code(code) => self.spill(&code),
            Node::Break => self.spill(" "),
            Node::Rule => self.spill("\n"),
            Node::Text(text) => self.spill(&text),
            Node::Element(..) => {}
        }
    }

    /// Keep `text` to draw after the container it couldn't go in
    fn spill(&mut self, text: &str) {
        if self.spill_at.is_none() {
            self.spill_at = Some(self.open.len());
        }
        self.spill.push_str(text);
    }

    /// Text, joined to the text before it: one text node, not one per
    /// event (the parser splits text at entities and brackets)
    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let text: CowStr<'_> = if text.contains('\0') {
            text.replace('\0', "").into()
        } else {
            text.into()
        };
        if self.skipping > 0 || !self.holds_text() {
            self.spill(&text);
            return;
        }
        let children = self.children();
        match children.last_mut() {
            Some(Node::Text(last)) => last.push_str(&text),
            _ => children.push(Node::Text(text.into_string())),
        }
    }

    /// Where a left-out block was: one line break, however many blocks
    /// were left out there in a row
    fn line_break(&mut self) {
        if self.skipping > 0 || !self.holds_text() {
            if !self.spill.is_empty() && !self.spill.ends_with('\n') {
                self.spill("\n");
            }
            return;
        }
        match self.children().last() {
            Some(Node::Text(last)) if last.ends_with('\n') => {}
            _ => self.text("\n"),
        }
    }

    /// A link's element, its `href`, title and `rel` decided
    fn link(
        &mut self,
        link_type: LinkType,
        dest: CowStr<'_>,
        title: CowStr<'_>,
    ) -> Kind {
        let mut href = String::new();
        if link_type == LinkType::Email {
            href.push_str("mailto:");
        }
        let (dest, agora) = on_site(&dest);
        escape_href(&mut href, &dest);
        let href = allowed(&href).then_some(href);
        let rel = match &href {
            Some(href) if agora || is_on_site(href, self.site.as_ref()) => {
                SITE_LINK_REL
            }
            _ => LINK_REL,
        };
        // As a browser reads an attribute's NUL, and the server's sanitiser
        // writes it
        let title =
            (!title.is_empty()).then(|| title.replace('\0', "\u{fffd}"));
        let cost = href.as_ref().map_or(0, String::len)
            + title.as_ref().map_or(0, String::len);
        if cost > self.link_bytes {
            return Kind::Link {
                href: None,
                title: None,
                rel: LINK_REL,
            };
        }
        self.link_bytes -= cost;
        Kind::Link { href, title, rel }
    }
}

/// A code block's class: its info string's first word, if [`LANGUAGES`]
/// names it
fn language(kind: &CodeBlockKind<'_>) -> Option<&'static str> {
    let CodeBlockKind::Fenced(info) = kind else {
        return None;
    };
    let word = info.split(' ').next().unwrap_or_default();
    LANGUAGES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(word))
        .map(|(_, class)| *class)
}

/// An `agora://` link's page path, as the server's writer has it, and
/// whether it was one; any other destination as it is. One that doesn't
/// parse stays as written, and is then refused as an unknown scheme
fn on_site(dest: &str) -> (CowStr<'_>, bool) {
    if !dest
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("agora://"))
    {
        return (dest.into(), false);
    }
    match dest.parse::<AgoraLink>() {
        Ok(link) => (link.site_path().into(), true),
        Err(_) => (dest.into(), false),
    }
}

/// Whether an allowed `href` stays on Agora: a fragment, a query, a path
/// from the root (`/…`, not `//host`, which is another site), or, with
/// `site` known, an `http(s)` URL on it with no user or password: the
/// site's origin as [`AgoraLink::parse_at`] reads a page URL (`http://` on
/// an `https://` site's host too), compared as parsed, so case and a
/// default port are the site's and a trailing dot, another port or a host
/// that merely begins with the site's are not. Not every relative URL:
/// `page.html` and `//host/x` are relative too
pub fn is_on_site(href: &str, site: Option<&Url>) -> bool {
    match href.as_bytes() {
        [b'#' | b'?', ..] => true,
        [b'/', next, ..] => *next != b'/',
        [b'/'] => true,
        _ => site.is_some_and(|site| {
            Url::parse(href).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.username().is_empty()
                    && url.password().is_none()
                    && crate::links::same_site(&url, site)
            })
        }),
    }
}

/// Whether `href` may be a link's `href`: relative, or with a scheme in
/// [`LINK_SCHEMES`]. As the server's sanitiser decides it: [`url::Url`]
/// (the WHATWG parser browsers implement) reads it, and anything else it
/// refuses is refused
pub(crate) fn allowed(href: &str) -> bool {
    match url::Url::parse(href) {
        Ok(url) => LINK_SCHEMES.contains(&url.scheme()),
        Err(url::ParseError::RelativeUrlWithoutBase) => true,
        Err(_) => false,
    }
}

/// `dest` percent-encoded into `out` as pulldown-cmark's HTML writer
/// encodes an `href` (`pulldown_cmark_escape::escape_href`), less its
/// HTML escaping of `&` and `'`, which dioxus-ssr does. A `\` becomes
/// `%5C`, so `\\host` is never read as `//host`
fn escape_href(out: &mut String, dest: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in dest.as_bytes() {
        let safe =
            b.is_ascii_alphanumeric() || b"!#$%&'()*+,-./:;=?@^_~".contains(&b);
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0xF)] as char);
        }
    }
}

/// `text` in pieces of at most [`TEXT_CHUNK`] bytes, cut at characters
fn chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(TEXT_CHUNK);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, after) = rest.split_at(end);
        rest = after;
        Some(piece)
    })
}

/// One node, drawn
pub(crate) fn node(node: &Node) -> Element {
    let (kind, children) = match node {
        Node::Text(text) => {
            return rsx! {
                for piece in chunks(text) {
                    "{piece}"
                }
            };
        }
        Node::Code(code) => {
            return rsx! {
                code {
                    for piece in chunks(code) {
                        "{piece}"
                    }
                }
            };
        }
        Node::Break => return rsx! { br {} },
        Node::Rule => return rsx! { hr {} },
        Node::Element(kind, children) => (kind, children),
    };
    let inner = children.iter().map(self::node);
    match kind {
        Kind::Paragraph => rsx! { p { {inner} } },
        Kind::Heading(HeadingLevel::H1) => rsx! { h1 { {inner} } },
        Kind::Heading(HeadingLevel::H2) => rsx! { h2 { {inner} } },
        Kind::Heading(HeadingLevel::H3) => rsx! { h3 { {inner} } },
        Kind::Heading(HeadingLevel::H4) => rsx! { h4 { {inner} } },
        Kind::Heading(HeadingLevel::H5) => rsx! { h5 { {inner} } },
        Kind::Heading(HeadingLevel::H6) => rsx! { h6 { {inner} } },
        Kind::Quote => rsx! { blockquote { {inner} } },
        Kind::CodeBlock(class) => rsx! {
            pre {
                code { class: *class, {inner} }
            }
        },
        Kind::List(None) => rsx! { ul { {inner} } },
        Kind::List(Some(1)) => rsx! { ol { {inner} } },
        Kind::List(Some(start)) => rsx! { ol { start: "{start}", {inner} } },
        Kind::Item => rsx! { li { {inner} } },
        Kind::Table => {
            // The header row first, as the parser sends it, then the body,
            // always present, as the HTML parser would insert it
            let head = children
                .iter()
                .filter(|n| matches!(n, Node::Element(Kind::Head, _)))
                .map(self::node);
            let rows = children
                .iter()
                .filter(|n| !matches!(n, Node::Element(Kind::Head, _)))
                .map(self::node);
            rsx! {
                table {
                    {head}
                    tbody { {rows} }
                }
            }
        }
        Kind::Head => rsx! { thead { tr { {inner} } } },
        Kind::Row => rsx! { tr { {inner} } },
        Kind::Cell { head: true } => rsx! { th { {inner} } },
        Kind::Cell { head: false } => rsx! { td { {inner} } },
        Kind::Emphasis => rsx! { em { {inner} } },
        Kind::Strong => rsx! { strong { {inner} } },
        Kind::Strikethrough => rsx! { del { {inner} } },
        Kind::Link { href, title, rel } => rsx! {
            a {
                href: href.clone(),
                title: title.clone(),
                rel: *rel,
                {inner}
            }
        },
    }
}

/// `source`'s text without its formatting, cut at a word to at most
/// `max_bytes` (and an ellipsis): a card's preview of a body. Text is
/// joined as written (`a [b] c`, which the parser gives in pieces, stays
/// so), a line break or the end of a block is a space, and every run of
/// whitespace is one space
pub fn plain_preview(source: &str, max_bytes: usize) -> String {
    use pulldown_cmark::TagEnd;
    let mut plain = String::new();
    for event in Parser::new(source) {
        match event {
            Event::Text(text) | Event::Code(text) => plain.push_str(&text),
            Event::SoftBreak | Event::HardBreak => plain.push(' '),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::CodeBlock
                | TagEnd::BlockQuote(_),
            ) => plain.push(' '),
            _ => {}
        }
    }
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = plain.as_str();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let end = trimmed
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_bytes)
        .last()
        .unwrap_or(0);
    let cut = &trimmed[..end];
    match cut.rfind(' ') {
        Some(at) => format!("{}…", &cut[..at]),
        None => format!("{cut}…"),
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
