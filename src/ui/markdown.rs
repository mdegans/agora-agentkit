//! Post and comment bodies: markdown drawn as Dioxus elements.
//!
//! pulldown-cmark's events are folded into a small tree ([`Node`]) and each
//! node is drawn by [`node`], one `rsx!` per element. Nothing writes HTML,
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
//! - Every `<a>` has `rel` [`LINK_REL`] and nothing else but `href` and
//!   `title`; no element has a `class`, `style` or `id`.
//!
//! And what keeps the server's tree the browser's (hydration adopts nodes
//! by position, so the HTML parser must not restructure what was drawn):
//!
//! - No link inside a link: an image inside a link is drawn as its alt
//!   text, in the outer link.
//! - Nesting stops at [`MAX_DEPTH`] elements: deeper elements are left
//!   out and their contents kept, in place. A table or list is left out
//!   whole when its rows or items wouldn't fit, never cut in the middle.
//! - The bytes of `href`s and titles are bounded by the source's length,
//!   so a reference link used ten thousand times can't multiply the page.
//!   Past the bound a link loses its `href` and title.
//!
//! The parser's options are the server's ([`options`]): tables and
//! strikethrough. Footnotes, task lists, heading ids, math and smart
//! punctuation are off, so `[^1]`, `[ ]` and `{#id}` are text, as there.

use dioxus::prelude::*;
use pulldown_cmark::{
    CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag,
};

use crate::links::AgoraLink;

/// The schemes a link may have, agora-server's sanitiser's (Steward,
/// 2026-10-23). A destination without one (a path, a `#fragment`) is
/// relative, and kept
pub const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// Every link's `rel`: a peer's link passes no ranking and no referrer
pub const LINK_REL: &str = "noopener noreferrer nofollow ugc";

/// The deepest a body's elements nest; deeper ones are left out and their
/// contents kept. Far below a browser's own limit (Chromium's parser stops
/// nesting at 512), with room for the page around the body
pub const MAX_DEPTH: usize = 32;

/// The parser's options, agora-server's: tables and strikethrough
pub fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// `source` as markdown, drawn as elements: a fragment of blocks, for the
/// caller's container (`div { class: "markdown-body", … }`)
#[component]
pub fn Markdown(source: String) -> Element {
    let nodes = parse(&source);
    rsx! {
        {nodes.iter().map(node)}
    }
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
    CodeBlock,
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

    /// Whether leaving it out should keep a space where it was, so the
    /// words either side don't run together
    fn is_block(&self) -> bool {
        !matches!(
            self,
            Kind::Emphasis
                | Kind::Strong
                | Kind::Strikethrough
                | Kind::Link { .. }
        )
    }
}

/// An open start tag: an element being filled, or one left out
#[derive(Debug)]
enum Frame {
    Open(Kind, Vec<Node>),
    /// Left out (too deep, a link in a link, or not drawn at all); its
    /// contents go to the nearest open element. `block`: keep a space;
    /// `deep`: counted in [`Builder::flattening`]
    LeftOut {
        block: bool,
        deep: bool,
    },
}

/// The tree's builder: an explicit stack, so a deep document can't
/// overflow the call stack
struct Builder {
    root: Vec<Node>,
    frames: Vec<Frame>,
    /// How many [`Frame::Open`] are on the stack
    depth: usize,
    /// How many of those are links
    links: usize,
    /// While above zero, every start tag is left out (one was too deep)
    flattening: usize,
    /// Bytes of `href`s and titles still allowed
    budget: usize,
}

/// `source`'s tree
pub(crate) fn parse(source: &str) -> Vec<Node> {
    let mut b = Builder {
        root: Vec::new(),
        frames: Vec::new(),
        depth: 0,
        links: 0,
        flattening: 0,
        budget: source.len().saturating_mul(3).saturating_add(4096),
    };
    for event in Parser::new_ext(source, options()) {
        b.event(event);
    }
    // The parser closes what it opens; anything left is closed here
    while !b.frames.is_empty() {
        b.end();
    }
    b.root
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(_) => self.end(),
            // Raw HTML is shown as written, never interpreted
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                self.text(&t)
            }
            Event::Code(t) => self.push(Node::Code(t.into_string())),
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
        let kind = match tag {
            Tag::Paragraph => Some(Kind::Paragraph),
            Tag::Heading { level, .. } => Some(Kind::Heading(level)),
            Tag::BlockQuote(_) => Some(Kind::Quote),
            Tag::CodeBlock(_) => Some(Kind::CodeBlock),
            Tag::List(start) => Some(Kind::List(start)),
            Tag::Item => Some(Kind::Item),
            Tag::Table(_) => Some(Kind::Table),
            Tag::TableHead => Some(Kind::Head),
            Tag::TableRow => Some(Kind::Row),
            Tag::TableCell => Some(Kind::Cell {
                head: matches!(self.open_kind(), Some(Kind::Head)),
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
            } => {
                (self.links == 0).then(|| self.link(link_type, dest_url, title))
            }
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
        if self.flattening > 0 || self.depth + kind.levels() > MAX_DEPTH {
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
        self.depth += 1;
        self.frames.push(Frame::Open(kind, Vec::new()));
    }

    fn end(&mut self) {
        match self.frames.pop() {
            Some(Frame::Open(kind, children)) => {
                self.depth -= 1;
                if matches!(kind, Kind::Link { .. }) {
                    self.links -= 1;
                }
                self.push(Node::Element(kind, children));
            }
            Some(Frame::LeftOut { block, deep }) => {
                if deep {
                    self.flattening -= 1;
                }
                if block {
                    self.text("\n");
                }
            }
            None => {}
        }
    }

    /// The kind of the innermost open element
    fn open_kind(&self) -> Option<&Kind> {
        self.frames.iter().rev().find_map(|f| match f {
            Frame::Open(kind, _) => Some(kind),
            Frame::LeftOut { .. } => None,
        })
    }

    /// Where a child goes: the innermost open element, or the root
    fn children(&mut self) -> &mut Vec<Node> {
        self.frames
            .iter_mut()
            .rev()
            .find_map(|f| match f {
                Frame::Open(_, children) => Some(children),
                Frame::LeftOut { .. } => None,
            })
            .unwrap_or(&mut self.root)
    }

    fn push(&mut self, node: Node) {
        self.children().push(node);
    }

    /// Text, joined to the text before it: one text node, not one per
    /// event (the parser splits text at entities and brackets)
    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let children = self.children();
        match children.last_mut() {
            Some(Node::Text(last)) => last.push_str(text),
            _ => children.push(Node::Text(text.to_string())),
        }
    }

    /// A link's element, its `href` and title decided
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
        escape_href(&mut href, &on_site(&dest));
        let href = allowed(&href).then_some(href);
        let title = (!title.is_empty()).then(|| title.into_string());
        let cost = href.as_ref().map_or(0, String::len)
            + title.as_ref().map_or(0, String::len);
        if cost > self.budget {
            return Kind::Link {
                href: None,
                title: None,
            };
        }
        self.budget -= cost;
        Kind::Link { href, title }
    }
}

/// An `agora://` link's page path, as the server's writer has it; any
/// other destination as it is. One that doesn't parse stays as written,
/// and is then refused as an unknown scheme
fn on_site(dest: &str) -> CowStr<'_> {
    if !dest
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("agora://"))
    {
        return dest.into();
    }
    match dest.parse::<AgoraLink>() {
        Ok(link) => link.site_path().into(),
        Err(_) => dest.into(),
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
            b.is_ascii_alphanumeric() || b"!#$%&'()*+,-./:;=?@_~".contains(&b);
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0xF)] as char);
        }
    }
}

/// One node, drawn
pub(crate) fn node(node: &Node) -> Element {
    let (kind, children) = match node {
        Node::Text(text) => return rsx! { "{text}" },
        Node::Code(code) => return rsx! { code { "{code}" } },
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
        Kind::CodeBlock => rsx! { pre { code { {inner} } } },
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
        Kind::Link { href, title } => rsx! {
            a {
                href: href.clone(),
                title: title.clone(),
                rel: LINK_REL,
                {inner}
            }
        },
    }
}

/// `source`'s text without its formatting, cut at a word to at most
/// `max_bytes` (and an ellipsis): a card's preview of a body. The
/// server's (`web::markdown::plain_preview`), moved here so the card draws
/// the same on both sides
pub fn plain_preview(source: &str, max_bytes: usize) -> String {
    let mut plain = String::new();
    for event in Parser::new(source) {
        match event {
            Event::Text(text) | Event::Code(text) => {
                plain.push_str(&text);
                plain.push(' ');
            }
            Event::SoftBreak | Event::HardBreak => plain.push(' '),
            _ => {}
        }
    }
    let trimmed = plain.trim();
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
