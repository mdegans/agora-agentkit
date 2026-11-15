use std::time::{Duration, Instant};

use super::*;
use crate::ui::ssr::pre_render;

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
/// what they draw instead (canonical form) and why
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
];

/// Every input in the corpus draws what the server draws, up to how it is
/// written (entities, attribute order, the newlines the server's writer
/// puts between blocks, the hydration markers), but for [`DIFFERENCES`]
#[test]
fn the_corpus_draws_what_the_server_draws() {
    let mut failures = Vec::new();
    let corpus = corpus();
    for case in &corpus {
        let ours = canonical(&render(&case.input));
        let theirs = canonical(&case.server);
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

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
}

/// HTML as a reader gets it, written one way: comments and hydration
/// markers dropped, entities decoded, attributes sorted, adjacent text
/// joined, and outside `<pre>` the newlines next to a block's tags (the
/// server writer's layout) dropped. A test's tokenizer, for the two
/// writers' output only: both quote attributes and escape `<` in text
fn canonical(html: &str) -> String {
    let mut tokens: Vec<Token> = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").expect("a closed comment");
            rest = &after[end + 3..];
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').unwrap();
            tokens.push(Token::End(after[..end].trim().to_string()));
            rest = &after[end + 1..];
        } else if rest.starts_with('<') {
            let (token, len) = start_tag(rest);
            tokens.push(token);
            rest = &rest[len..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = decode(&rest[..end]);
            match tokens.last_mut() {
                Some(Token::Text(last)) => last.push_str(&text),
                _ => tokens.push(Token::Text(text)),
            }
            rest = &rest[end..];
        }
    }
    // Comments dropped can leave texts side by side
    let mut joined: Vec<Token> = Vec::new();
    for token in tokens {
        match (joined.last_mut(), token) {
            (Some(Token::Text(last)), Token::Text(t)) => last.push_str(&t),
            (_, token) => joined.push(token),
        }
    }
    let is_block = |t: Option<&Token>| match t {
        Some(Token::Start(name, _) | Token::End(name)) => {
            BLOCKS.contains(&name.as_str())
        }
        _ => false,
    };
    let mut out = String::new();
    let mut pre = 0usize;
    for (i, token) in joined.iter().enumerate() {
        match token {
            Token::Start(name, attrs) => {
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
            Token::End(name) => {
                if name == "pre" {
                    pre = pre.saturating_sub(1);
                }
                out.push_str(&format!("</{name}>"));
            }
            Token::Text(text) => {
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
        }
    }
    out
}

/// A start tag at the front of `s`, and its length
fn start_tag(s: &str) -> (Token, usize) {
    let end = s.find('>').unwrap();
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
            let close = v.find('"').unwrap();
            attrs_src = &v[close + 1..];
            decode(&v[..close])
        } else if let Some(v) = attrs_src.strip_prefix('=') {
            let close = v.find(char::is_whitespace).unwrap_or(v.len());
            attrs_src = &v[close..];
            decode(&v[..close])
        } else {
            String::new()
        };
        if key != "data-node-hydration" {
            attrs.push((key, value));
        }
    }
    attrs.sort();
    (Token::Start(name.to_string(), attrs), end + 1)
}

fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else { break };
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
        canonical("<p>a &amp; b</p>\n<p>c<br />\nd</p>\n"),
        canonical(
            r#"<p data-node-hydration="0"><!--node-id1-->a &#38; b<!--#--></p><p>c<br/>d</p>"#
        )
    );
    assert_ne!(canonical("<p>a</p>"), canonical("<p><em>a</em></p>"));
    assert_ne!(
        canonical(r#"<a href="x" rel="r">t</a>"#),
        canonical(r#"<a rel="r">t</a>"#)
    );
    // Inside `<pre>` every newline counts
    assert_ne!(
        canonical("<pre><code>\nx</code></pre>"),
        canonical("<pre><code>x</code></pre>")
    );
}

// -- What a body can never contain ------------------------------------------

/// Every element and attribute in `nodes`, checked: only the elements the
/// tree has, only `href` (allowed), `title`, `rel` and `start` attributes
fn check_tree(nodes: &[Node], depth: usize, max: &mut usize) {
    *max = (*max).max(depth);
    for node in nodes {
        if let Node::Element(kind, children) = node {
            if let Kind::Link {
                href: Some(href), ..
            } = kind
            {
                assert!(allowed(href), "{href}");
                assert!(!href.contains(['\\', ' ', '"', '<', '>']), "{href}");
                if let Ok(url) = url::Url::parse(href) {
                    assert!(LINK_SCHEMES.contains(&url.scheme()), "{href}");
                }
            }
            if matches!(kind, Kind::Link { .. }) {
                assert!(
                    !contains_link(children),
                    "a link in a link: {children:?}"
                );
            }
            check_tree(children, depth + 1, max);
        }
    }
}

fn contains_link(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Element(Kind::Link { .. }, _) => true,
        Node::Element(_, children) => contains_link(children),
        _ => false,
    })
}

/// The markup's attributes: only these names, on these elements
fn check_markup(html: &str) {
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = &after[after.find("-->").unwrap() + 3..];
            continue;
        }
        if rest.starts_with("</") {
            rest = &rest[2..];
            continue;
        }
        let (token, len) = start_tag(rest);
        let Token::Start(name, attrs) = token else {
            unreachable!()
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
        for (key, value) in &attrs {
            match (name.as_str(), key.as_str()) {
                ("a", "href") => assert!(allowed(value), "{value}"),
                ("a", "title") => {}
                ("a", "rel") => assert_eq!(value, LINK_REL),
                ("ol", "start") => {
                    assert!(value.parse::<u64>().is_ok(), "{value}")
                }
                _ => panic!("{key}={value:?} on <{name}> in {html}"),
            }
        }
        if name == "a" {
            assert!(attrs.iter().any(|(k, _)| k == "rel"), "{html}");
        }
        rest = &rest[len..];
    }
}

#[test]
fn nothing_in_the_corpus_escapes_the_policy() {
    for case in corpus() {
        let mut max = 0;
        check_tree(&parse(&case.input), 0, &mut max);
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
    "| <script> |\n|---|\n| <img src=x> |",
    "[x](https://a.example \"\\\" onmouseover=\\\"alert(1)\")",
    "[\"><script>alert(1)</script>](https://a.example)",
];

#[test]
fn hostile_bodies_are_inert() {
    for source in HOSTILE {
        let html = render(source);
        check_markup(&html);
        let mut max = 0;
        check_tree(&parse(source), 0, &mut max);
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

// -- Pathological documents -------------------------------------------------

fn max_depth(nodes: &[Node]) -> usize {
    let mut max = 0;
    check_tree(nodes, 0, &mut max);
    max
}

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
        let started = Instant::now();
        let nodes = parse(&source);
        assert!(max_depth(&nodes) <= MAX_DEPTH, "{}", max_depth(&nodes));
        let html = render(&source);
        assert!(html.contains(word), "{word}");
        assert!(started.elapsed() < Duration::from_secs(10));
        check_markup(&html);
    }
}

/// A table or list too deep to fit is left out whole, never cut: no row
/// without its table, no item without its list
#[test]
fn a_structure_at_the_limit_is_left_out_whole() {
    for quotes in MAX_DEPTH - 4..=MAX_DEPTH {
        let source = format!(
            "{}| a |\n{}|---|\n{}| 1 |\n\n{}- item\n  - sub",
            "> ".repeat(quotes),
            "> ".repeat(quotes),
            "> ".repeat(quotes),
            "> ".repeat(quotes)
        );
        let nodes = parse(&source);
        assert!(max_depth(&nodes) <= MAX_DEPTH);
        whole(&nodes, None);
        let html = render(&source);
        assert!(html.contains("-->a"), "{html}");
        assert!(html.contains("item"), "{html}");
    }

    fn whole(nodes: &[Node], parent: Option<&Kind>) {
        for node in nodes {
            if let Node::Element(kind, children) = node {
                match kind {
                    Kind::Head | Kind::Row => {
                        assert_eq!(parent, Some(&Kind::Table))
                    }
                    Kind::Cell { .. } => {
                        assert!(matches!(parent, Some(Kind::Head | Kind::Row)))
                    }
                    Kind::Item => {
                        assert!(matches!(parent, Some(Kind::List(_))))
                    }
                    _ => {}
                }
                whole(children, Some(kind));
            }
        }
    }
}

/// One long reference used thousands of times can't multiply the page:
/// past a bound on `href` and title bytes, links keep their text only
#[test]
fn reused_references_cannot_blow_up_the_output() {
    let url = format!("https://a.example/{}", "x".repeat(4_000));
    let source = format!(
        "[r]: {url} \"{}\"\n\n{}",
        "t".repeat(1_000),
        "[r] ".repeat(10_000)
    );
    let html = render(&source);
    assert!(html.len() < source.len() * 30, "{} bytes", html.len());
    // The first links are whole
    assert!(html.contains(&format!(r#"href="{url}""#)));
    check_markup(&html);
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

// -- The markup itself ------------------------------------------------------

/// The markup hydration adopts, markers and all: a change here moves the
/// server and the client together, in an agentkit release
#[test]
fn a_body_snapshot() {
    assert_eq!(
        render(
            "# Title\n\nSome *text* and [a link](https://a.example \"t\").\n\n- one\n- two\n\n```\ncode\n```"
        ),
        concat!(
            r#"<h1 data-node-hydration="0"><!--node-id1-->Title<!--#--></h1>"#,
            r#"<p data-node-hydration="2"><!--node-id3-->Some <!--#--><em data-node-hydration="4"><!--node-id5-->text<!--#--></em><!--node-id6--> and <!--#-->"#,
            r#"<a href="https://a.example" title="t" rel="noopener noreferrer nofollow ugc" data-node-hydration="7"><!--node-id8-->a link<!--#--></a><!--node-id9-->.<!--#--></p>"#,
            r#"<ul data-node-hydration="10"><li data-node-hydration="11"><!--node-id12-->one<!--#--></li><li data-node-hydration="13"><!--node-id14-->two<!--#--></li></ul>"#,
            r#"<pre data-node-hydration="15"><code><!--node-id16-->code"#,
            "\n",
            r#"<!--#--></code></pre>"#,
        )
    );
}

#[test]
fn an_unallowed_link_keeps_its_text_and_rel_only() {
    assert_eq!(
        canonical(&render("[x](javascript:alert(1) \"t\")")),
        r#"<p><a rel="noopener noreferrer nofollow ugc" title="t">x</a></p>"#
    );
}

#[test]
fn a_numbered_list_keeps_its_start() {
    assert_eq!(
        canonical(&render("7. seven\n8. eight")),
        r#"<ol start="7"><li>seven</li><li>eight</li></ol>"#
    );
    assert_eq!(canonical(&render("1. one")), "<ol><li>one</li></ol>");
}

#[test]
fn an_empty_body_draws_nothing() {
    assert_eq!(canonical(&render("")), "");
    assert_eq!(canonical(&render("  \n\n")), "");
}

// -- Previews ---------------------------------------------------------------

#[test]
fn a_preview_is_the_text_cut_at_a_word() {
    // The server's spacing: a space after every text event
    assert_eq!(
        plain_preview("**bold** and `code`\nnext", 200),
        "bold  and  code  next"
    );
    assert_eq!(plain_preview("one two three", 9), "one two…");
    assert_eq!(plain_preview("onetwothree", 5), "onetw…");
    // Never inside a character
    assert_eq!(plain_preview("ééééé", 3), "é…");
    // Raw HTML is left out, the text between its tags kept
    assert_eq!(plain_preview("<b>raw</b> text", 200), "raw  text");
}
