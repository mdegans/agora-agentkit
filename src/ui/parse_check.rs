//! Tests' check that an HTML parser reads the components' markup as the
//! tree that was drawn (tests under `dioxus-ssr` only).
//!
//! Hydration adopts the server's nodes by position: if the browser's
//! parser builds a different tree from the markup (splits a nested `<a>`,
//! closes a `<p>` before a block, moves text out of a `<table>`), every
//! node after it is misplaced, and nothing reports it. [`assert_reparses`]
//! parses the markup with html5ever (the HTML standard's tree builder, as
//! browsers implement it) as the contents of a `<div>`, and compares the
//! token streams, hydration markers (comments, `data-node-hydration`)
//! included. [`content_model`] checks the markup against the rules the
//! components keep, which a parser may accept silently.

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
};

use html5ever::{
    Attribute, LocalName, ParseOpts, QualName,
    interface::{ElementFlags, NodeOrText, QuirksMode, TreeSink},
    local_name, ns, parse_fragment,
    tendril::{StrTendril, TendrilSink},
};

/// One token of markup, as a reader's tree has it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
    Comment(String),
}

const VOID: &[&str] = &["br", "hr", "img", "input", "wbr"];

/// `html` (well-formed, as dioxus-ssr writes it) as tokens: entities
/// decoded, attributes sorted, voids without an end, empty texts dropped
pub fn tokens(html: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").expect("a closed comment");
            out.push(Tok::Comment(after[..end].to_string()));
            rest = &after[end + 3..];
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').expect("a closed end tag");
            let name = after[..end].trim().to_string();
            if !VOID.contains(&name.as_str()) {
                out.push(Tok::End(name));
            }
            rest = &after[end + 1..];
        } else if rest.starts_with('<') {
            let (token, len) = start_tag(rest);
            out.push(token);
            rest = &rest[len..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            out.push(Tok::Text(normalize(&decode(&rest[..end]))));
            rest = &rest[end..];
        }
    }
    out.retain(|t| !matches!(t, Tok::Text(s) if s.is_empty()));
    out
}

/// What the HTML input stream does to text before any parsing: line
/// endings become `\n`
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The start tag at the front of `s` (quoted values may hold `>`), and
/// its length
pub fn start_tag(s: &str) -> (Tok, usize) {
    let bytes = s.as_bytes();
    let mut end = 1;
    let mut quoted = false;
    while end < bytes.len() {
        match bytes[end] {
            b'"' => quoted = !quoted,
            b'>' if !quoted => break,
            _ => {}
        }
        end += 1;
    }
    let inner = s[1..end].trim_end_matches('/').trim();
    let (name, mut attrs_src) =
        inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
    let mut attrs = Vec::new();
    loop {
        attrs_src = attrs_src.trim_start();
        if attrs_src.is_empty() {
            break;
        }
        let key_end = attrs_src
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(attrs_src.len());
        let key = attrs_src[..key_end].to_string();
        attrs_src = &attrs_src[key_end..];
        let value = if let Some(v) = attrs_src.strip_prefix("=\"") {
            let close = v.find('"').expect("a closed value");
            attrs_src = &v[close + 1..];
            decode(&v[..close])
        } else if let Some(v) = attrs_src.strip_prefix('=') {
            let close = v.find(char::is_whitespace).unwrap_or(v.len());
            attrs_src = &v[close..];
            decode(&v[..close])
        } else {
            String::new()
        };
        attrs.push((key, normalize(&value)));
    }
    attrs.sort();
    (Tok::Start(name.to_string(), attrs), end + 1)
}

/// Character references decoded
pub fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|e| *e < 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .map(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').map(|d| d.parse().ok()))
                .flatten()
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// -- A minimal DOM for html5ever's tree builder -----------------------------

#[derive(Debug)]
enum Data {
    Document,
    Element(QualName, Vec<(String, String)>),
    Text(String),
    Comment(String),
    Other,
}

/// A node: what it is, its parent, its children
type Slot = (Data, Option<usize>, Vec<usize>);

#[derive(Debug)]
struct Dom {
    nodes: RefCell<Vec<Slot>>,
    errors: Cell<usize>,
}

impl Dom {
    fn new() -> Self {
        Self {
            nodes: RefCell::new(vec![(Data::Document, None, Vec::new())]),
            errors: Cell::new(0),
        }
    }

    fn add(&self, data: Data) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        nodes.push((data, None, Vec::new()));
        nodes.len() - 1
    }

    fn detach(&self, node: usize) {
        let mut nodes = self.nodes.borrow_mut();
        if let Some(parent) = nodes[node].1.take() {
            nodes[parent].2.retain(|c| *c != node);
        }
    }

    /// Insert `child` into `parent` at `at` (the end if `None`), joining a
    /// text to a text beside it, as a DOM does
    fn insert(
        &self,
        parent: usize,
        at: Option<usize>,
        child: NodeOrText<usize>,
    ) {
        let at = at.unwrap_or_else(|| self.nodes.borrow()[parent].2.len());
        match child {
            NodeOrText::AppendText(text) => {
                let mut nodes = self.nodes.borrow_mut();
                let before = at.checked_sub(1).map(|i| nodes[parent].2[i]);
                if let Some(before) = before
                    && let Data::Text(t) = &mut nodes[before].0
                {
                    t.push_str(&text);
                    return;
                }
                drop(nodes);
                let node = self.add(Data::Text(text.to_string()));
                let mut nodes = self.nodes.borrow_mut();
                nodes[node].1 = Some(parent);
                nodes[parent].2.insert(at, node);
            }
            NodeOrText::AppendNode(node) => {
                self.detach(node);
                let mut nodes = self.nodes.borrow_mut();
                let at = at.min(nodes[parent].2.len());
                nodes[node].1 = Some(parent);
                nodes[parent].2.insert(at, node);
            }
        }
    }

    fn walk(&self, node: usize, out: &mut Vec<Tok>) {
        let nodes = self.nodes.borrow();
        let (data, _, children) = &nodes[node];
        match data {
            Data::Element(name, attrs) => {
                let tag = name.local.to_string();
                out.push(Tok::Start(tag.clone(), attrs.clone()));
                let children = children.clone();
                drop(nodes);
                for child in children {
                    self.walk(child, out);
                }
                if !VOID.contains(&tag.as_str()) {
                    out.push(Tok::End(tag));
                }
            }
            Data::Text(t) => out.push(Tok::Text(t.clone())),
            Data::Comment(c) => out.push(Tok::Comment(c.clone())),
            Data::Document | Data::Other => {
                let children = children.clone();
                drop(nodes);
                for child in children {
                    self.walk(child, out);
                }
            }
        }
    }
}

#[derive(Debug)]
struct Name(QualName);

impl html5ever::interface::ElemName for Name {
    fn ns(&self) -> &html5ever::Namespace {
        &self.0.ns
    }
    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

impl<'d> TreeSink for &'d Dom {
    type Handle = usize;
    type Output = &'d Dom;
    type ElemName<'a>
        = Name
    where
        Self: 'a;

    fn finish(self) -> Self::Output {
        self
    }
    fn parse_error(&self, _msg: Cow<'static, str>) {
        self.errors.set(self.errors.get() + 1);
    }
    fn get_document(&self) -> usize {
        0
    }
    fn elem_name<'a>(&'a self, target: &'a usize) -> Name {
        match &self.nodes.borrow()[*target].0 {
            Data::Element(name, _) => Name(name.clone()),
            _ => panic!("not an element"),
        }
    }
    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> usize {
        let mut attrs: Vec<(String, String)> = attrs
            .into_iter()
            .map(|a| (a.name.local.to_string(), a.value.to_string()))
            .collect();
        attrs.sort();
        self.add(Data::Element(name, attrs))
    }
    fn create_comment(&self, text: StrTendril) -> usize {
        self.add(Data::Comment(text.to_string()))
    }
    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> usize {
        self.add(Data::Other)
    }
    fn append(&self, parent: &usize, child: NodeOrText<usize>) {
        self.insert(*parent, None, child);
    }
    fn append_based_on_parent_node(
        &self,
        element: &usize,
        prev_element: &usize,
        child: NodeOrText<usize>,
    ) {
        if self.nodes.borrow()[*element].1.is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }
    fn append_doctype_to_document(
        &self,
        _name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
    }
    fn get_template_contents(&self, target: &usize) -> usize {
        *target
    }
    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }
    fn set_quirks_mode(&self, _mode: QuirksMode) {}
    fn append_before_sibling(&self, sibling: &usize, child: NodeOrText<usize>) {
        let parent = self.nodes.borrow()[*sibling].1.expect("a parent");
        let at = self.nodes.borrow()[parent]
            .2
            .iter()
            .position(|c| c == sibling)
            .expect("a child of its parent");
        self.insert(parent, Some(at), child);
    }
    fn add_attrs_if_missing(&self, target: &usize, attrs: Vec<Attribute>) {
        if let Data::Element(_, have) = &mut self.nodes.borrow_mut()[*target].0
        {
            for a in attrs {
                let key = a.name.local.to_string();
                if !have.iter().any(|(k, _)| *k == key) {
                    have.push((key, a.value.to_string()));
                }
            }
            have.sort();
        }
    }
    fn remove_from_parent(&self, target: &usize) {
        self.detach(*target);
    }
    fn reparent_children(&self, node: &usize, new_parent: &usize) {
        let children = std::mem::take(&mut self.nodes.borrow_mut()[*node].2);
        for child in children {
            self.nodes.borrow_mut()[child].1 = None;
            self.insert(*new_parent, None, NodeOrText::AppendNode(child));
        }
    }
}

/// `html` parsed as a `<div>`'s contents, as tokens
pub fn reparsed(html: &str) -> Vec<Tok> {
    let dom = Dom::new();
    let context = QualName::new(None, ns!(html), local_name!("div"));
    let dom =
        parse_fragment(&dom, ParseOpts::default(), context, vec![], false)
            .one(html);
    let mut out = Vec::new();
    dom.walk(0, &mut out);
    // The fragment's own `<html>` wrapper
    if matches!(out.first(), Some(Tok::Start(name, _)) if name == "html") {
        out.remove(0);
        out.pop();
    }
    out
}

/// What a parser makes of `html` is what was drawn: the same tokens,
/// markers included
#[track_caller]
pub fn assert_reparses(html: &str) {
    let ours = tokens(html);
    let theirs = reparsed(html);
    if ours != theirs {
        let at = ours
            .iter()
            .zip(&theirs)
            .position(|(a, b)| a != b)
            .unwrap_or(ours.len().min(theirs.len()));
        panic!(
            "a parser rebuilds this markup at token {at}: drawn {:?}, parsed {:?}\n{html}",
            ours.get(at),
            theirs.get(at)
        );
    }
}

/// Elements that hold only phrasing content
const PHRASING_ONLY: &[&str] = &[
    "p", "h1", "h2", "h3", "h4", "h5", "h6", "a", "em", "strong", "del",
    "code", "span",
];

/// Block elements the components draw
const BLOCKS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "hr",
    "div",
    "section",
    "article",
];

/// The rules the components keep, which a parser may not enforce: lists
/// hold items, tables rows and rows cells, no block inside phrasing, no
/// link in a link, no text where a table's or list's parts go. A
/// complaint, or none
pub fn content_model(html: &str) -> Option<String> {
    let mut stack: Vec<String> = Vec::new();
    for tok in tokens(html) {
        match tok {
            Tok::Start(name, _) => {
                let n = name.as_str();
                let parent = stack.last().map(String::as_str);
                let bad = match n {
                    "li" => !matches!(parent, Some("ul" | "ol")),
                    "thead" | "tbody" => parent != Some("table"),
                    "tr" => !matches!(parent, Some("thead" | "tbody")),
                    "th" | "td" => parent != Some("tr"),
                    "a" => stack.iter().any(|s| s == "a"),
                    _ => false,
                } || match parent {
                    Some("ul" | "ol") => n != "li",
                    Some("table") => !matches!(n, "thead" | "tbody"),
                    Some("thead" | "tbody") => n != "tr",
                    Some("tr") => !matches!(n, "th" | "td"),
                    Some("pre") => n != "code",
                    _ => false,
                } || (BLOCKS.contains(&n)
                    && stack
                        .iter()
                        .any(|s| PHRASING_ONLY.contains(&s.as_str())));
                if bad {
                    return Some(format!("<{n}> under {stack:?}"));
                }
                if !VOID.contains(&n) {
                    stack.push(name);
                }
            }
            Tok::End(name) => {
                if stack.pop().as_deref() != Some(name.as_str()) {
                    return Some(format!("misnested </{name}>"));
                }
            }
            Tok::Text(text) => {
                if matches!(
                    stack.last().map(String::as_str),
                    Some(
                        "ul" | "ol"
                            | "table"
                            | "thead"
                            | "tbody"
                            | "tr"
                            | "pre"
                    )
                ) {
                    return Some(format!(
                        "text {text:?} directly under {stack:?}"
                    ));
                }
            }
            Tok::Comment(_) => {}
        }
    }
    (!stack.is_empty()).then(|| format!("unclosed {stack:?}"))
}

/// Both checks
#[track_caller]
pub fn assert_hydratable(html: &str) {
    if let Some(why) = content_model(html) {
        panic!("{why}\n{html}");
    }
    assert_reparses(html);
}

#[test]
fn the_check_sees_what_a_parser_rebuilds() {
    // Fine
    assert_hydratable(
        r#"<p data-node-hydration="0"><!--node-id1-->a &#38; b<!--#--></p><table><thead><tr><th>a</th></tr></thead><tbody></tbody></table>"#,
    );
    // A link in a link is split; a block in a `<p>` closes it; text in a
    // table is moved out; a row with no `<tbody>` gets one
    for bad in [
        r#"<a href="x"><a href="y">t</a></a>"#,
        "<p><div>x</div></p>",
        "<table>text<tbody></tbody></table>",
        "<table><tr><td>x</td></tr></table>",
    ] {
        assert_ne!(tokens(bad), reparsed(bad), "{bad}");
    }
    assert!(content_model("<ul>text</ul>").is_some());
    assert!(content_model("<p><a><a></a></a></p>").is_some());
}
