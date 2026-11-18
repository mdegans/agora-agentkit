use crate::ui::island::Anonymous as _;
use std::time::{Duration, Instant};

use super::*;
use crate::ui::{
    parse_check::{Tok, assert_hydratable, content_model, tokens},
    ssr::pre_render,
};

fn render(source: &str) -> String {
    pre_render(
        Markdown,
        MarkdownProps {
            source: source.to_string(),
        },
    )
}

// -- The parity corpus ------------------------------------------------------

/// One case of `corpus.json`: an input, and what agora-server's renderer
/// drew for it (recorded by `tools/markdown-parity`, `just markdown-corpus`)
#[derive(serde::Deserialize)]
struct Case {
    name: String,
    input: String,
    server: String,
}

fn corpus() -> Vec<Case> {
    serde_json::from_str(include_str!("corpus.json")).unwrap()
}

/// The cases where the components deliberately draw something else, with
/// what they draw instead (canonical form) and why. Not listed: a link to
/// Agora's own pages has `rel` [`SITE_LINK_REL`] here, where the server
/// puts `nofollow ugc` on every link; [`canonical`] makes that change to
/// the server's markup before comparing
const DIFFERENCES: &[(&str, &str)] = &[
    // The server's sanitiser re-parses `<a><a>…</a></a>` and splits it: the
    // outer link is left empty and the image's link takes the text. A
    // browser would split the components' tree the same way and hydration
    // would misplace every node after it, so the image is its alt text, in
    // the outer link
    (
        "image in a link",
        r#"<p><a href="https://site.example" rel="noopener noreferrer nofollow ugc">alt</a></p>"#,
    ),
    (
        "link text with a link-like image",
        r#"<p><a href="https://site.example/report" rel="noopener noreferrer nofollow ugc">see the chart here</a></p>"#,
    ),
    // The same for a link inside an image's alt text: the image's link
    // keeps all of its text
    (
        "link in an image",
        r#"<p><a href="https://img.example/i.png" rel="noopener noreferrer nofollow ugc">a b c</a></p>"#,
    ),
    // A known language keeps its class (Steward, 2026-11-15); the server's
    // sanitiser strips every class
    (
        "fenced code with a language",
        r#"<pre><code class="language-rust">fn main() { println!("&lt;hi&gt;"); }
</code></pre>"#,
    ),
    (
        "markdown code block",
        r#"<pre><code class="language-markdown"># not a heading
[not](a-link)
</code></pre>"#,
    ),
];

/// Every input in the corpus draws what the server draws, up to how it is
/// written (entities, attribute order, the newlines the server's writer
/// puts between blocks, the hydration markers), but for [`DIFFERENCES`]
#[test]
fn the_corpus_draws_what_the_server_draws() {
    let mut failures = Vec::new();
    let corpus = corpus();
    for case in &corpus {
        let ours = canonical(&render(&case.input), false);
        let theirs = canonical(&case.server, true);
        // Past TABLE_CELLS, rows are drawn without their trailing empty
        // cells, where the server draws them all
        let theirs = if case.name.starts_with("above the table cap") {
            let mut t = theirs;
            while t.contains("<td></td></tr>") {
                t = t.replace("<td></td></tr>", "</tr>");
            }
            assert_ne!(t, canonical(&case.server, true), "{}", case.name);
            t
        } else {
            theirs
        };
        match DIFFERENCES.iter().find(|(name, _)| *name == case.name) {
            Some((_, want)) => {
                assert_ne!(ours, theirs, "{}: no longer differs", case.name);
                if ours != *want {
                    failures.push(format!(
                        "{}\n  ours: {ours}\n  want: {want}",
                        case.name
                    ));
                }
            }
            None if ours != theirs => failures.push(format!(
                "{}\n  ours:   {ours}\n  server: {theirs}",
                case.name
            )),
            None => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    for (name, _) in DIFFERENCES {
        assert!(corpus.iter().any(|c| c.name == *name), "{name}");
    }
}

/// The unhydrated HTML is the same body, without the markers
#[test]
fn to_html_draws_the_corpus_without_markers() {
    for case in corpus() {
        let html = to_html(&case.input, None);
        assert!(!html.contains("<!--"), "{html}");
        assert!(!html.contains("data-node-hydration"), "{html}");
        assert_eq!(
            canonical(&html, false),
            canonical(&render(&case.input), false),
            "{}",
            case.name
        );
    }
}

/// The corpus is the server's tests' inputs and more, each recorded
#[test]
fn the_corpus_is_recorded() {
    let corpus = corpus();
    assert!(corpus.len() >= 80, "{}", corpus.len());
    assert!(
        corpus
            .iter()
            .filter(|c| c.name.starts_with("server: "))
            .count()
            >= 13
    );
    for case in &corpus {
        assert!(
            !case.server.is_empty() || case.input.trim().is_empty(),
            "{}: not recorded (`just markdown-corpus`)",
            case.name
        );
    }
}

/// Tags whose neighbouring newlines are the server writer's layout, not
/// text a reader sees
const BLOCKS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "hr",
    "br",
];

/// HTML as a reader gets it, written one way: comments and hydration
/// markers dropped, entities decoded, attributes sorted, adjacent text
/// joined, and outside `<pre>` the newlines next to a block's tags (the
/// server writer's layout) dropped. With `site_rel`, a link to Agora's
/// own pages gets [`SITE_LINK_REL`], as the components give it
fn canonical(html: &str, site_rel: bool) -> String {
    let mut joined: Vec<Tok> = Vec::new();
    for token in tokens(html) {
        let token = match token {
            Tok::Comment(_) => continue,
            Tok::Start(name, attrs) => {
                let href = attrs
                    .iter()
                    .find(|(k, _)| k == "href")
                    .map(|(_, v)| v.clone());
                let attrs = attrs
                    .into_iter()
                    .filter(|(k, _)| k != "data-node-hydration")
                    .map(|(k, v)| match (&href, k.as_str()) {
                        (Some(href), "rel")
                            if site_rel && is_on_site(href, None) =>
                        {
                            (k, SITE_LINK_REL.to_string())
                        }
                        _ => (k, v),
                    })
                    .collect();
                Tok::Start(name, attrs)
            }
            other => other,
        };
        match (joined.last_mut(), token) {
            (Some(Tok::Text(last)), Tok::Text(t)) => last.push_str(&t),
            (_, token) => joined.push(token),
        }
    }
    let is_block = |t: Option<&Tok>| match t {
        Some(Tok::Start(name, _) | Tok::End(name)) => {
            BLOCKS.contains(&name.as_str())
        }
        _ => false,
    };
    let mut out = String::new();
    let mut pre = 0usize;
    for (i, token) in joined.iter().enumerate() {
        match token {
            Tok::Start(name, attrs) => {
                if name == "pre" {
                    pre += 1;
                }
                out.push('<');
                out.push_str(name);
                for (k, v) in attrs {
                    out.push_str(&format!(" {k}=\"{}\"", escape(v, true)));
                }
                out.push('>');
            }
            Tok::End(name) => {
                if name == "pre" {
                    pre = pre.saturating_sub(1);
                }
                out.push_str(&format!("</{name}>"));
            }
            Tok::Text(text) => {
                let mut text = text.as_str();
                if pre == 0 {
                    if i == 0 || is_block(joined.get(i - 1)) {
                        text = text.trim_start_matches('\n');
                    }
                    if is_block(joined.get(i + 1)) || i + 1 == joined.len() {
                        text = text.trim_end_matches('\n');
                    }
                }
                out.push_str(&escape(text, false));
            }
            Tok::Comment(_) => {}
        }
    }
    out
}

fn escape(s: &str, attr: bool) -> String {
    let s = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    if attr { s.replace('"', "&quot;") } else { s }
}

#[test]
fn the_canonical_form_ignores_only_how_it_is_written() {
    assert_eq!(
        canonical("<p>a &amp; b</p>\n<p>c<br />\nd</p>\n", false),
        canonical(
            r#"<p data-node-hydration="0"><!--node-id1-->a &#38; b<!--#--></p><p>c<br/>d</p>"#,
            false
        )
    );
    assert_ne!(
        canonical("<p>a</p>", false),
        canonical("<p><em>a</em></p>", false)
    );
    assert_ne!(
        canonical(r#"<a href="x" rel="r">t</a>"#, false),
        canonical(r#"<a rel="r">t</a>"#, false)
    );
    // Inside `<pre>` every newline counts
    assert_ne!(
        canonical("<pre><code>\nx</code></pre>", false),
        canonical("<pre><code>x</code></pre>", false)
    );
    // A `>` in a quoted value doesn't end the tag
    assert_eq!(
        canonical(r#"<a title="a > b" rel="r">t</a>"#, false),
        r#"<a rel="r" title="a &gt; b">t</a>"#
    );
}

// -- What a body can never contain ------------------------------------------

/// The tree's own rules: links allowed and never nested, depth bounded,
/// and the content model the HTML parser expects (a table holds its head
/// and rows, which hold cells; a list holds items; a paragraph, heading
/// or inline element holds no block; nothing but cells in a row)
fn check_tree(nodes: &[Node], parent: Option<&Kind>, depth: usize) -> usize {
    let mut max = depth;
    for node in nodes {
        let kind = match node {
            Node::Element(kind, _) => Some(kind),
            _ => None,
        };
        match parent {
            Some(Kind::Table) => assert!(
                matches!(kind, Some(Kind::Head | Kind::Row)),
                "{node:?} in a table"
            ),
            Some(Kind::Head | Kind::Row) => assert!(
                matches!(kind, Some(Kind::Cell { .. })),
                "{node:?} in a row"
            ),
            Some(Kind::List(_)) => {
                assert!(matches!(kind, Some(Kind::Item)), "{node:?} in a list")
            }
            Some(
                Kind::Paragraph
                | Kind::Heading(_)
                | Kind::Emphasis
                | Kind::Strong
                | Kind::Strikethrough
                | Kind::Link { .. }
                | Kind::Cell { .. },
            ) => assert!(
                kind.is_none_or(|k| !k.is_block()),
                "{node:?} in phrasing"
            ),
            Some(Kind::CodeBlock(_)) => {
                assert!(matches!(node, Node::Text(_)), "{node:?} in code")
            }
            _ => {}
        }
        match kind {
            Some(Kind::Head | Kind::Row) => {
                assert_eq!(parent, Some(&Kind::Table), "{node:?}")
            }
            Some(Kind::Cell { .. }) => assert!(
                matches!(parent, Some(Kind::Head | Kind::Row)),
                "{node:?}"
            ),
            Some(Kind::Item) => {
                assert!(matches!(parent, Some(Kind::List(_))), "{node:?}")
            }
            _ => {}
        }
        if let Node::Text(text) = node {
            assert!(!text.contains('\0'));
        }
        if let Node::Element(kind, children) = node {
            if let Kind::Link { href, title, rel } = kind {
                if let Some(href) = href {
                    assert!(allowed(href), "{href}");
                    assert!(
                        !href.contains(['\\', ' ', '"', '<', '>']),
                        "{href}"
                    );
                    if let Ok(url) = url::Url::parse(href) {
                        assert!(LINK_SCHEMES.contains(&url.scheme()), "{href}");
                    }
                    if *rel == SITE_LINK_REL {
                        assert!(is_on_site(href, None), "{href}");
                    }
                } else {
                    assert_eq!(*rel, LINK_REL);
                }
                if let Some(title) = title {
                    assert!(!title.contains('\0'));
                }
                assert!(
                    !contains_link(children),
                    "a link in a link: {children:?}"
                );
            }
            if let Kind::CodeBlock(Some(class)) = kind {
                assert!(LANGUAGES.iter().any(|(_, c)| c == class));
            }
            max = max.max(check_tree(children, Some(kind), depth + 1));
        }
    }
    max
}

fn contains_link(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Element(Kind::Link { .. }, _) => true,
        Node::Element(_, children) => contains_link(children),
        _ => false,
    })
}

/// The markup's elements and attributes: only these names, on these
/// elements; and an HTML parser reads it as drawn
fn check_markup(html: &str) {
    for token in tokens(html) {
        let Tok::Start(name, attrs) = token else {
            continue;
        };
        assert!(
            [
                "p",
                "h1",
                "h2",
                "h3",
                "h4",
                "h5",
                "h6",
                "blockquote",
                "pre",
                "code",
                "ul",
                "ol",
                "li",
                "table",
                "thead",
                "tbody",
                "tr",
                "th",
                "td",
                "em",
                "strong",
                "del",
                "a",
                "br",
                "hr"
            ]
            .contains(&name.as_str()),
            "<{name}> in {html}"
        );
        let href = attrs.iter().find(|(k, _)| k == "href").map(|(_, v)| v);
        for (key, value) in &attrs {
            match (name.as_str(), key.as_str()) {
                (_, "data-node-hydration") => {}
                ("a", "href") => assert!(allowed(value), "{value}"),
                ("a", "title") => {}
                ("a", "rel") => match href {
                    Some(href) if is_on_site(href, None) => {
                        assert_eq!(value, SITE_LINK_REL)
                    }
                    _ => assert_eq!(value, LINK_REL),
                },
                ("ol", "start") => {
                    assert!(value.parse::<u64>().is_ok(), "{value}")
                }
                ("code", "class") => assert!(
                    LANGUAGES.iter().any(|(_, c)| c == value),
                    "{value}"
                ),
                _ => panic!("{key}={value:?} on <{name}> in {html}"),
            }
        }
        if name == "a" {
            assert!(attrs.iter().any(|(k, _)| k == "rel"), "{html}");
        }
    }
    assert_hydratable(html);
}

#[test]
fn nothing_in_the_corpus_escapes_the_policy() {
    for case in corpus() {
        check_tree(&parse(&case.input), None, 0);
        check_markup(&render(&case.input));
    }
}

/// Strings an attacker would write in a body, beyond the corpus
const HOSTILE: &[&str] = &[
    "<script>alert(1)</script>",
    "<img src=x onerror=alert(1)>",
    "[x](javascript:alert(1))",
    "[x](JAVASCRIPT:alert(1))",
    "[x]( javascript:alert(1) )",
    "[x](java\tscript:alert(1))",
    "[x](javascript&colon;alert(1))",
    "[x](&#x6a;avascript:alert(1))",
    "[x](data:text/html;base64,PHNjcmlwdD4=)",
    "[x](vbscript:msgbox(1))",
    "[x](blob:https://a.example/1)",
    "[x](\u{0}javascript:alert(1))",
    "[x](\u{1}javascript:alert(1))",
    "[x](%6Aavascript:alert(1))",
    "[x](\\\\evil.example)",
    "![x](javascript:alert(1))",
    "<javascript:alert(1)>",
    "[a]: javascript:alert(1)\n\n[a]",
    "`<script>`",
    "```\n</code></pre><script>alert(1)</script>\n```",
    "``` onload=alert(1)\nx\n```",
    "```language-rust\" onclick=\"x\nx\n```",
    "| <script> |\n|---|\n| <img src=x> |",
    "[x](https://a.example \"\\\" onmouseover=\\\"alert(1)\")",
    "[\"><script>alert(1)</script>](https://a.example)",
    "nul \0 in text [t](/x \"nul\0title\") `\0`",
];

#[test]
fn hostile_bodies_are_inert() {
    for source in HOSTILE {
        let html = render(source);
        check_markup(&html);
        check_tree(&parse(source), None, 0);
        assert!(!html.contains('\0'), "{html:?}");
        for bad in ["<script", "<img", "<iframe", "javascript:", "onerror="] {
            // Present only as escaped text, never as markup or an href
            if let Some(at) = html.find(bad) {
                let before = &html[..at];
                let open = before.rfind('<').unwrap_or(0);
                let close = before.rfind('>').unwrap_or(0);
                assert!(close >= open, "{bad} inside a tag: {html}");
            }
        }
    }
}

// -- Links ------------------------------------------------------------------

/// Agora's own pages lose `nofollow ugc`; everything else, `//host` and a
/// bare relative path included, keeps it
#[test]
fn only_links_to_agoras_own_pages_drop_nofollow() {
    for (source, href, rel) in [
        (
            "[x](agora://post/7ad26ccd)",
            Some("/agora/post/7ad26ccd"),
            SITE_LINK_REL,
        ),
        ("[x](/agora/feed)", Some("/agora/feed"), SITE_LINK_REL),
        // A page no other kind names (0.168): its page
        (
            "[x](agora://page/hello-world?step=2)",
            Some("/agora/hello-world?step=2"),
            SITE_LINK_REL,
        ),
        ("[x](/)", Some("/"), SITE_LINK_REL),
        ("[x](#comment-1)", Some("#comment-1"), SITE_LINK_REL),
        ("[x](?q=1)", Some("?q=1"), SITE_LINK_REL),
        ("[x](//evil.example/x)", Some("//evil.example/x"), LINK_REL),
        (
            "[x](/\\evil.example)",
            Some("/%5Cevil.example"),
            SITE_LINK_REL,
        ),
        ("[x](\\\\evil.example)", Some("%5Cevil.example"), LINK_REL),
        ("[x](page.html)", Some("page.html"), LINK_REL),
        (
            "[x](https://subliminal.technology/agora/feed)",
            Some("https://subliminal.technology/agora/feed"),
            LINK_REL,
        ),
        ("[x](mailto:a@b.c)", Some("mailto:a@b.c"), LINK_REL),
        ("<a@b.c>", Some("mailto:a@b.c"), LINK_REL),
        ("[x](javascript:alert(1))", None, LINK_REL),
        ("[x](agora://nonsense/at/all)", None, LINK_REL),
    ] {
        let nodes = parse(source);
        let [Node::Element(Kind::Paragraph, children)] = &nodes[..] else {
            panic!("{nodes:?}");
        };
        let [
            Node::Element(
                Kind::Link {
                    href: got,
                    rel: got_rel,
                    ..
                },
                _,
            ),
        ] = &children[..]
        else {
            panic!("{children:?}");
        };
        assert_eq!(got.as_deref(), href, "{source}");
        assert_eq!(*got_rel, rel, "{source}");
    }
    assert!(
        !is_on_site("//x", None)
            && !is_on_site("", None)
            && is_on_site("/", None)
    );
}

/// The bound on `href` and title bytes binds before pulldown-cmark's own
/// on reference expansion (`max(source, 100 KB)`): a 2 KB destination
/// used 40 times is 80 KB of `href`s, allowed by the parser
#[test]
fn the_link_byte_bound_binds() {
    let url = format!("https://a.example/{}", "x".repeat(2_000));
    let source = format!("[r]: {url}\n\n{}", "[r] ".repeat(40));
    let nodes = parse(&source);
    let mut with_href = 0;
    let mut without = 0;
    fn count(nodes: &[Node], with: &mut usize, without: &mut usize) {
        for node in nodes {
            if let Node::Element(kind, children) = node {
                match kind {
                    Kind::Link { href: Some(_), .. } => *with += 1,
                    Kind::Link { href: None, .. } => *without += 1,
                    _ => {}
                }
                count(children, with, without);
            }
        }
    }
    count(&nodes, &mut with_href, &mut without);
    let budget = source.len() * 3;
    assert_eq!(with_href, budget / url.len(), "{with_href}");
    assert_eq!(with_href + without, 40);
}

#[test]
fn a_known_language_has_its_class_and_nothing_else_does() {
    for (source, class) in [
        ("```rust\nx\n```", Some("language-rust")),
        ("```RUST\nx\n```", Some("language-rust")),
        ("```markdown\n# x\n```", Some("language-markdown")),
        ("```rust ignore\nx\n```", Some("language-rust")),
        ("```rust,ignore\nx\n```", None),
        ("```language-rust\nx\n```", None),
        ("```\nx\n```", None),
        ("    indented\n", None),
        ("```\"><script>\nx\n```", None),
    ] {
        let nodes = parse(source);
        let [Node::Element(Kind::CodeBlock(got), _)] = &nodes[..] else {
            panic!("{nodes:?}");
        };
        assert_eq!(*got, class, "{source}");
    }
    for (name, class) in LANGUAGES {
        assert_eq!(*class, format!("language-{name}"));
        assert!(name.bytes().all(|b| b.is_ascii_lowercase()), "{name}");
    }
}

/// `^` is one of the characters pulldown-cmark's writer leaves as it is
#[test]
fn hrefs_are_encoded_as_the_servers_writer_encodes_them() {
    let mut out = String::new();
    escape_href(&mut out, "a^b c\\d\"e|f`g{h}é");
    assert_eq!(out, "a^b%20c%5Cd%22e%7Cf%60g%7Bh%7D%C3%A9");
}

// -- Pathological documents -------------------------------------------------

#[test]
fn deep_nesting_is_bounded_and_keeps_its_text() {
    for (source, word) in [
        (format!("{}deep", "> ".repeat(10_000)), "deep"),
        (format!("{}deep", "* ".repeat(5_000)), "deep"),
        (format!("{}deep", "1. ".repeat(5_000)), "deep"),
        (
            (0..2_000)
                .map(|i| format!("{}- level {i}\n", "  ".repeat(i)))
                .collect(),
            "level 1999",
        ),
        (
            format!("{}deep{}", "*a ".repeat(3_000), " a*".repeat(3_000)),
            "deep",
        ),
        (
            format!("{}deep{}", "[".repeat(3_000), "](x)".repeat(3_000)),
            "deep",
        ),
    ] {
        let nodes = parse(&source);
        assert!(check_tree(&nodes, None, 0) <= MAX_DEPTH);
        let html = render(&source);
        assert!(html.contains(word), "{word}");
        check_markup(&html);
    }
}

/// The left-out blocks of a deep list leave one line break, not one each
#[test]
fn left_out_blocks_leave_one_line_break() {
    let nodes = parse(&format!("{}x", "- ".repeat(30_000)));
    fn longest(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.matches('\n').count(),
                Node::Element(_, c) => longest(c),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
    assert!(longest(&nodes) <= 2, "{}", longest(&nodes));
}

/// The text of `html`, joined
fn text_of(html: &str) -> String {
    tokens(html)
        .into_iter()
        .filter_map(|t| match t {
            Tok::Text(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// A table or list too deep to fit is left out whole, never cut: no row
/// without its table, no item without its list, and none of its words
/// lost. Around the limit, for every container that could cut it
#[test]
fn a_structure_at_the_limit_is_left_out_whole() {
    for depth in MAX_DEPTH - 5..=MAX_DEPTH {
        for prefix in ["> ", "- ", "1. "] {
            let lead = prefix.repeat(depth);
            let cont: String = std::iter::repeat_n(
                match prefix {
                    "> " => "> ",
                    "- " => "  ",
                    _ => "   ",
                },
                depth,
            )
            .collect();
            let source = format!(
                "{lead}| alpha | beta |\n{cont}|---|---|\n{cont}| one1 | two2 |\n\n{cont}- item\n{cont}  - sub\n{cont}    - subsub"
            );
            let nodes = parse(&source);
            assert!(check_tree(&nodes, None, 0) <= MAX_DEPTH, "{source}");
            let html = render(&source);
            check_markup(&html);
            let text = text_of(&html);
            for word in ["alpha", "beta", "one1", "two2", "item", "subsub"] {
                assert!(
                    text.contains(word),
                    "{depth} {prefix:?} {word}: {html}"
                );
            }
        }
    }
}

/// Whatever the events, nothing goes where a table's or list's parts go:
/// a paragraph or text the parser sends inside a table or a list (as it
/// does after a table that ran out of padding) is drawn after it, as text
#[test]
fn nothing_but_parts_go_in_a_table_or_list() {
    use pulldown_cmark::{Alignment, TagEnd};
    let events = [
        Event::Start(Tag::Table(vec![Alignment::None])),
        Event::Text("loose".into()),
        Event::Start(Tag::Paragraph),
        Event::Text("in a table".into()),
        Event::End(TagEnd::Paragraph),
        Event::Start(Tag::TableRow),
        Event::Start(Tag::Paragraph),
        Event::End(TagEnd::Paragraph),
        Event::Start(Tag::TableCell),
        Event::Text("cell".into()),
        Event::End(TagEnd::TableCell),
        Event::End(TagEnd::TableRow),
        Event::End(TagEnd::Table),
        Event::Start(Tag::List(None)),
        Event::Text("loose".into()),
        Event::Start(Tag::TableCell),
        Event::End(TagEnd::TableCell),
        Event::Start(Tag::Item),
        Event::Text("item".into()),
        Event::End(TagEnd::Item),
        Event::End(TagEnd::List(false)),
        // A part outside its container
        Event::Start(Tag::Item),
        Event::Text("stray".into()),
        Event::End(TagEnd::Item),
    ];
    let mut b = Builder::new(1_000, None);
    for event in events {
        b.event(event);
    }
    let nodes = b.finish();
    check_tree(&nodes, None, 0);
    let html = std::iter::once(&nodes).map(|n| {
        let mut dom =
            VirtualDom::new_with_props(Tree, TreeProps { nodes: n.clone() });
        dom.rebuild_in_place();
        dioxus_ssr::pre_render(&dom)
    });
    for html in html {
        check_markup(&html);
        let text = text_of(&html);
        assert!(text.contains("cell") && text.contains("item"), "{html}");
        // Nothing is lost: what had nowhere to go follows its container
        for word in ["loose", "in a table", "stray"] {
            assert!(text.contains(word), "{word}: {html}");
        }
        let table_end = html.find("</table>").unwrap();
        assert!(html.find("in a table").unwrap() > table_end, "{html}");
    }

    #[component]
    fn Tree(nodes: Vec<Node>) -> Element {
        rsx! { {nodes.iter().map(node)} }
    }
}

fn elements(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::Element(_, c) => 1 + elements(c),
            _ => 0,
        })
        .sum()
}

/// A short table under a wide header: pulldown-cmark pads every row to
/// the header's width, which would be 682 cells a row from a 4 KiB body.
/// The element bound stops it at one element per byte of source
#[test]
fn padded_cells_are_bounded_by_the_source() {
    let wide = |bytes: usize| {
        let n = bytes / 8;
        let mut s = "|".repeat(n + 1);
        s.push('\n');
        s.push_str(&"|-".repeat(n));
        s.push_str("|\n");
        let rest = bytes.saturating_sub(s.len());
        s.push_str(&"a\n".repeat(rest / 2));
        s
    };
    for bytes in [512, 4_096, 65_536] {
        let source = wide(bytes);
        let nodes = parse(&source);
        assert!(
            elements(&nodes) <= source.len() + ELEMENT_ALLOWANCE + TABLE_CELLS,
            "{} elements from {} bytes",
            elements(&nodes),
            source.len()
        );
        check_tree(&nodes, None, 0);
        if bytes <= 4_096 {
            let html = render(&source);
            assert!(html.len() < source.len() * 100, "{} bytes", html.len());
            check_markup(&html);
        }
    }
    // A page of ten such bodies is ten times one, no more
    let source = wide(4_096);
    let one = render(&source).len();
    let page: usize = (0..10).map(|_| render(&source).len()).sum();
    assert_eq!(page, one * 10);
    assert!(page < 10 * source.len() * 100);
}

/// An ordinary document is nowhere near the bound
#[test]
fn ordinary_documents_keep_every_element() {
    let source = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n\n- one\n- two\n  - three\n\n> *quoted* **text** ~~old~~";
    let nodes = parse(source);
    let html = render(source);
    assert_eq!(html.matches("<td").count(), 6, "{html}");
    assert!(elements(&nodes) < source.len());
}

/// Time to parse `source`, the least of five
fn time_to_parse(source: &str) -> Duration {
    (0..5)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(parse(source));
            started.elapsed()
        })
        .min()
        .unwrap()
}

/// Linear in the source: sixteen times the input takes about sixteen
/// times as long, where a scan per event would take 256. A ratio of the
/// best of five, not a time, and a wide one (fails at 64): on a loaded,
/// shared box a ratio of a few is noise, a factor of sixteen isn't. The
/// builder has no loop over its stacks to count instead
#[test]
fn building_is_linear() {
    for unit in ["- ", "> ", "*a ", "**a *b ", "[a](", "|"] {
        let small = unit.repeat(8_192 / unit.len());
        let large = unit.repeat(16 * 8_192 / unit.len());
        let (s, l) = (time_to_parse(&small), time_to_parse(&large));
        let ratio = l.as_secs_f64() / s.as_secs_f64().max(1e-6);
        assert!(ratio < 64.0, "{unit:?}: {s:?} then {l:?} ({ratio:.1}x)");
    }
}

/// The documented limit, exactly: a larger one must be measured against
/// the stack again (`drawing_at_the_limit_fits_a_small_stack`) and the
/// docs changed with it
#[test]
fn the_depth_limit_is_the_documented_one() {
    assert_eq!(MAX_DEPTH, 32);
}

/// Drawing recurses once per level of the tree. At [`MAX_DEPTH`] it fits
/// in 64 KiB in a release build (measured: under 64 KiB; a debug build's
/// frames need about 700 KiB, which is why the debug run gets wasm's
/// default 1 MiB)
#[test]
fn drawing_at_the_limit_fits_a_small_stack() {
    let source = format!("{}*a **b ~~c [d](/x)~~** e*", "> ".repeat(MAX_DEPTH));
    let html = std::thread::Builder::new()
        .stack_size(if cfg!(debug_assertions) { 1024 } else { 64 } * 1024)
        .spawn(move || render(&source))
        .unwrap()
        .join()
        .expect("no stack overflow");
    assert!(html.contains("<blockquote"));
}

/// A long text is drawn as several text nodes, none over [`TEXT_CHUNK`]
/// bytes, cut at characters, and reads back whole
#[test]
fn long_texts_are_drawn_in_pieces() {
    let text = "é".repeat(30_000) + &"a".repeat(10_001);
    for source in [
        text.clone(),
        format!("`{text}`"),
        format!("```\n{text}\n```"),
    ] {
        let html = render(&source);
        let texts: Vec<String> = tokens(&html)
            .into_iter()
            .filter_map(|t| match t {
                Tok::Text(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(texts.len() >= 4, "{}", texts.len());
        assert!(texts.iter().all(|t| t.len() <= TEXT_CHUNK));
        assert!(texts.concat().contains(&text));
        check_markup(&html);
    }
}

#[test]
fn a_long_body_renders_in_reasonable_time() {
    let paragraph = "Some *emphasis*, a [link](https://a.example/x), `code` \
                     and <b>raw html</b> &amp; entities.\n\n";
    let source = paragraph.repeat(1_000_000 / paragraph.len());
    let started = Instant::now();
    let html = render(&source);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(
        html.matches("<p ").count() + html.matches("<p>").count(),
        1_000_000 / paragraph.len()
    );
}

/// The same source draws the same markup, every time: nothing drawn reads
/// the clock, the locale or a hash map's order
#[test]
fn rendering_is_deterministic() {
    for case in corpus() {
        assert_eq!(render(&case.input), render(&case.input), "{}", case.name);
    }
}

// -- Generated documents ----------------------------------------------------

/// xorshift64: the same documents on every run
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Pieces of markdown, joined at random: the reviewer's generator for
/// #313, cut down. `tools/markdown-parity`'s `fuzz` runs the full one
const ATOMS: &[&str] = &[
    "a",
    "b",
    "word ",
    " ",
    "\n",
    "\n\n",
    "\t",
    "\r\n",
    "\0",
    "*",
    "**",
    "_",
    "~~",
    "`",
    "```",
    "```\n",
    "```rust\n",
    "    ",
    "[",
    "]",
    "(",
    ")",
    "](/x)",
    "](https://a.example)",
    "](javascript:x)",
    "](//h)",
    "![",
    "[r]",
    "[r]: /u \"t\"\n",
    "<",
    ">",
    "<a>",
    "</a>",
    "<a href=\"x\">",
    "<b>",
    "<div>\n",
    "<pre>",
    "<table>",
    "<tr>",
    "<td>",
    "<p>",
    "</p>",
    "<!--",
    "-->",
    "<![CDATA[",
    "<?",
    "<script>",
    "<https://a.example>",
    "<a@b.c>",
    "> ",
    "- ",
    "* ",
    "1. ",
    "7. ",
    "# ",
    "###### ",
    "---\n",
    "===\n",
    "|",
    " | ",
    "|---|",
    "| a | b |\n",
    "|:-:|--:|\n",
    "\\",
    "  \n",
    "&amp;",
    "&#0;",
    "&nbsp;",
    "&",
    "'",
    "\"",
    "é",
    "\u{202e}",
    "\u{2028}",
    "[^1]",
    "- [ ] ",
];

fn generated(rng: &mut Rng) -> String {
    let mut s = String::new();
    if rng.below(4) == 0 {
        let n = MAX_DEPTH - 8 + rng.below(12);
        for _ in 0..n {
            s.push_str(["> ", "- ", "1. "][rng.below(3)]);
        }
    }
    let prefix = s.replace("- ", "  ").replace("1. ", "   ");
    for _ in 0..1 + rng.below(40) {
        let atom = ATOMS[rng.below(ATOMS.len())];
        s.push_str(atom);
        if atom.ends_with('\n') && rng.below(3) == 0 {
            s.push_str(&prefix);
        }
    }
    s
}

/// A few thousand generated documents: each draws markup an HTML parser
/// reads as drawn (markers included), keeps the content model and the
/// link policy, and stays within the bounds
#[test]
fn generated_documents_hydrate() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..3_000 {
        let source = generated(&mut rng);
        let nodes = parse(&source);
        assert!(check_tree(&nodes, None, 0) <= MAX_DEPTH, "{source:?}");
        assert!(
            elements(&nodes) <= source.len() + ELEMENT_ALLOWANCE + TABLE_CELLS
        );
        let html = render(&source);
        if let Some(why) = content_model(&html) {
            panic!("{why}\n{source:?}\n{html}");
        }
        check_markup(&html);
    }
}

// -- The markup itself ------------------------------------------------------

/// The markup hydration adopts, markers and all: a change here moves the
/// server and the client together, in an agentkit release
#[test]
fn a_body_snapshot() {
    assert_eq!(
        render(
            "# Title\n\nSome *text* and [a link](https://a.example \"t\").\n\n- one\n- two\n\n```rust\ncode\n```"
        ),
        concat!(
            r#"<h1 data-node-hydration="0"><!--node-id1-->Title<!--#--></h1>"#,
            r#"<p data-node-hydration="2"><!--node-id3-->Some <!--#--><em data-node-hydration="4"><!--node-id5-->text<!--#--></em><!--node-id6--> and <!--#-->"#,
            r#"<a href="https://a.example" title="t" rel="noopener noreferrer nofollow ugc" data-node-hydration="7"><!--node-id8-->a link<!--#--></a><!--node-id9-->.<!--#--></p>"#,
            r#"<ul data-node-hydration="10"><li data-node-hydration="11"><!--node-id12-->one<!--#--></li><li data-node-hydration="13"><!--node-id14-->two<!--#--></li></ul>"#,
            r#"<pre data-node-hydration="15"><code class="language-rust" data-node-hydration="16"><!--node-id17-->code"#,
            "\n",
            r#"<!--#--></code></pre>"#,
        )
    );
}

#[test]
fn an_unallowed_link_keeps_its_text_and_rel_only() {
    assert_eq!(
        canonical(&render("[x](javascript:alert(1) \"t\")"), false),
        r#"<p><a rel="noopener noreferrer nofollow ugc" title="t">x</a></p>"#
    );
}

#[test]
fn a_numbered_list_keeps_its_start() {
    assert_eq!(
        canonical(&render("7. seven\n8. eight"), false),
        r#"<ol start="7"><li>seven</li><li>eight</li></ol>"#
    );
    assert_eq!(canonical(&render("1. one"), false), "<ol><li>one</li></ol>");
}

#[test]
fn an_empty_body_draws_nothing() {
    assert_eq!(canonical(&render(""), false), "");
    assert_eq!(canonical(&render("  \n\n"), false), "");
}

// -- Previews ---------------------------------------------------------------

#[test]
fn a_preview_is_the_text_cut_at_a_word() {
    // Text as written; a line break is a space
    assert_eq!(
        plain_preview("**bold** and `code`\nnext", 200),
        "bold and code next"
    );
    // The parser gives `[`, `b` and `]` as three events: joined as written
    assert_eq!(plain_preview("a [b] c", 200), "a [b] c");
    assert_eq!(plain_preview("[removed]", 200), "[removed]");
    // Blocks are a space apart, and runs of whitespace one space
    assert_eq!(
        plain_preview(
            "# Head\n\nOne.\n\n- a\n- b\n\n```\nx  y\n```\n\n> q",
            200
        ),
        "Head One. a b x y q"
    );
    assert_eq!(plain_preview("a\u{a0} \t b", 200), "a b");
    assert_eq!(plain_preview("one two three", 9), "one two…");
    assert_eq!(plain_preview("onetwothree", 5), "onetw…");
    // Never inside a character
    assert_eq!(plain_preview("ééééé", 3), "é…");
    // Raw HTML is left out, the text between its tags kept
    assert_eq!(plain_preview("<b>raw</b> text", 200), "raw text");
}

/// A short table under a wide header, written honestly, then more: every
/// row is drawn padded, as the server draws it, and so is everything after
/// it. The padding costs nothing
#[test]
fn an_honest_ragged_table_keeps_every_row_and_what_follows() {
    let mut source =
        format!("{}|\n{}|\n", "| h ".repeat(12), "|---".repeat(12));
    for i in 0..60 {
        source.push_str(&format!("| row{i} |\n"));
    }
    source.push_str("\nAfter *the* [table](https://a.example).\n\n- one\n- two\n\n| x | y |\n|---|---|\n| second | table |\n");
    assert!(source.len() < 1_000, "{}", source.len());
    let html = render(&source);
    check_markup(&html);
    for i in 0..60 {
        assert!(html.contains(&format!(">row{i}<")), "row{i}: {html}");
    }
    // Padded as the server pads it: twelve cells a row
    assert_eq!(html.matches("<td").count(), 12 * 60 + 2, "{html}");
    for want in [
        "<em",
        "<a href=\"https://a.example\"",
        "<li",
        ">second<",
        ">table<",
    ] {
        assert!(html.contains(want), "{want}: {html}");
    }
}

/// Where the budget binds on hostile input, no text is lost: rows that
/// can't be drawn have their words drawn after the table
#[test]
fn where_the_budget_binds_the_words_survive() {
    // 4 KiB of a 682-column header, then one-cell rows, then a second table
    let n = 682;
    let mut source = format!("{}|\n{}|\n", "|".repeat(n), "|-".repeat(n));
    let mut i = 0;
    while source.len() < 4_000 {
        source.push_str(&format!("w{i}\n"));
        i += 1;
    }
    source.push_str("\n| x | y |\n|---|---|\n| second | table |\n");
    let nodes = parse(&source);
    assert!(elements(&nodes) <= source.len() + ELEMENT_ALLOWANCE + TABLE_CELLS);
    let html = render(&source);
    check_markup(&html);
    let text = text_of(&html);
    for j in 0..i {
        assert!(text.contains(&format!("w{j}")), "w{j}");
    }
    assert!(text.contains("second") && text.contains("table"), "{html}");
    assert!(html.len() < source.len() * 100, "{} bytes", html.len());
}

/// The element budget is a backstop no honest input meets; driven here
/// with a small one: every word survives, the structure stays valid, and
/// no more elements are drawn than allowed (padding aside)
#[test]
fn a_small_element_budget_keeps_every_word() {
    let source = "# Title\n\nSome *emphasis* and [a link](/x).\n\n- one\n- two\n  - three\n\n| a | b |\n|---|---|\n| c | d |\n| e | f |\n\n> quoted **text**\n\nlast words";
    for budget in [0, 1, 2, 3, 5, 8, 13] {
        let mut b = Builder::new(source.len(), None);
        b.elements = budget;
        for event in Parser::new_ext(source, options()) {
            b.event(event);
        }
        let nodes = b.finish();
        check_tree(&nodes, None, 0);
        assert!(elements(&nodes) <= budget, "{budget}: {}", elements(&nodes));
        let html = {
            let mut dom = VirtualDom::new_with_props(Tree, TreeProps { nodes });
            dom.rebuild_in_place();
            dioxus_ssr::pre_render(&dom)
        };
        check_markup(&html);
        let text = text_of(&html);
        for word in [
            "Title",
            "emphasis",
            "a link",
            "one",
            "two",
            "three",
            "a",
            "b",
            "c",
            "d",
            "e",
            "f",
            "quoted",
            "text",
            "last words",
        ] {
            assert!(text.contains(word), "{budget}: {word}: {html}");
        }
    }

    #[component]
    fn Tree(nodes: Vec<Node>) -> Element {
        rsx! { {nodes.iter().map(node)} }
    }
}

/// With the site known, an absolute link to it is the site's own: its
/// origin as parsed, so nothing that only looks like it
#[test]
fn an_absolute_link_to_the_site_is_on_it() {
    let site = Url::parse("https://subliminal.technology").unwrap();
    for href in [
        "https://subliminal.technology/agora/post/7ad26ccd",
        "https://subliminal.technology",
        "https://subliminal.technology/other",
        "HTTPS://SUBLIMINAL.TECHNOLOGY/agora/feed",
        "https://subliminal.technology:443/agora/feed",
        "https://subliminal%2Etechnology/agora/feed",
        // The site's host over http, as a page URL reads it
        "http://subliminal.technology/agora/feed",
        "/agora/feed",
    ] {
        assert!(is_on_site(href, Some(&site)), "{href}");
    }
    for href in [
        "https://subliminal.technology.evil.example/agora/feed",
        "https://subliminal.technology@evil.example/agora/feed",
        "https://subliminal.technology:443@evil.example/",
        "https://evil.example/https://subliminal.technology/",
        "https://evil.example/?subliminal.technology",
        "https://user@subliminal.technology/agora/feed",
        "https://user:pw@subliminal.technology/agora/feed",
        "https://:pw@subliminal.technology/agora/feed",
        "https://subliminal.technology:8443/agora/feed",
        "http://subliminal.technology:8080/agora/feed",
        "https://subliminal.technology./agora/feed",
        "https://git.subliminal.technology/agora-agentkit",
        "https://technology/agora/feed",
        "mailto:steward@subliminal.technology",
        "ftp://subliminal.technology/agora/feed",
        "//subliminal.technology/agora/feed",
    ] {
        assert!(!is_on_site(href, Some(&site)), "{href}");
    }
    // With none known, only a relative link is
    assert!(!is_on_site(
        "https://subliminal.technology/agora/feed",
        None
    ));
}

/// A body's absolute link to the site gets the site's `rel` where the
/// page says which site it is on, and only there
#[test]
fn a_body_on_the_site_links_to_it_as_its_own() {
    let site = Url::parse("https://subliminal.technology").unwrap();
    let rel = |source: &str, site: Option<&Url>| -> Vec<&'static str> {
        fn rels(nodes: &[Node], out: &mut Vec<&'static str>) {
            for node in nodes {
                if let Node::Element(kind, children) = node {
                    if let Kind::Link { rel, .. } = kind {
                        out.push(*rel);
                    }
                    rels(children, out);
                }
            }
        }
        let mut out = Vec::new();
        rels(&parse_at(source, site), &mut out);
        out
    };
    let source = "[ours](https://subliminal.technology/agora/post/7ad26ccd) \
                  [theirs](https://subliminal.technology.evil.example/) \
                  [path](/agora/feed)";
    assert_eq!(
        rel(source, Some(&site)),
        [SITE_LINK_REL, LINK_REL, SITE_LINK_REL]
    );
    assert_eq!(rel(source, None), [LINK_REL, LINK_REL, SITE_LINK_REL]);
}

/// The site reaches a body through the page's data, so the server's
/// render and the client's read the same one
#[cfg(feature = "dioxus-ssr")]
#[test]
fn the_site_comes_from_the_page() {
    use crate::ui::thread::{ThreadLoad, ThreadPage};
    let site = Url::parse("https://subliminal.technology").unwrap();
    let mut read = crate::fixtures::post_with_comments();
    read.post.body =
        "[ours](https://subliminal.technology/agora/feed)".to_owned();
    let page =
        ThreadPage::new(read.post.id, ThreadLoad::Loaded(Box::new(read)));
    let ours = format!("rel=\"{SITE_LINK_REL}\"");
    let off = format!("rel=\"{LINK_REL}\"");
    let at = crate::ui::page::Page::from(page.clone().at(&site))
        .public()
        .render()
        .unwrap();
    assert!(
        at.html.contains(&ours) && !at.html.contains(&off),
        "{}",
        at.html
    );
    // The data block holds the site, for the client's first render
    let back: crate::ui::page::Page = crate::ui::island::from_script_text(
        at.script
            .split_once('>')
            .unwrap()
            .1
            .rsplit_once("</script>")
            .unwrap()
            .0,
    )
    .unwrap();
    assert!(
        matches!(back, crate::ui::page::Page::Thread(ThreadPage { site: Some(ref s), .. }) if *s == site)
    );
    let nowhere = crate::ui::page::Page::from(page).public().render().unwrap();
    assert!(nowhere.html.contains(&off), "{}", nowhere.html);
    assert_eq!(
        to_html("<https://subliminal.technology/x>", Some(&site)),
        to_html("[https://subliminal.technology/x](/x)", None)
            .replace("href=\"/x\"", "href=\"https://subliminal.technology/x\"")
    );
}
