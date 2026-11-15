//! agora-server's markdown renderer, copied (`server`), and the HTML
//! parser round trip and content-model check the fuzz run applies to
//! `ui::markdown`'s markup. From the review of #313 (2026-11-15).
#![allow(dead_code)]

use agora_agentkit::ui::{
    markdown::{Markdown, MarkdownProps},
    ssr::pre_render,
};
use std::sync::LazyLock;

pub fn render(source: &str) -> String {
    pre_render(
        Markdown,
        MarkdownProps {
            source: source.to_string(),
        },
    )
}

// The server's renderer, copied
static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    b.url_schemes(["http", "https", "mailto"].into())
        .link_rel(Some("noopener noreferrer nofollow ugc"));
    b
});
pub fn server_unsanitized(input: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(input, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: on_site(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: on_site(dest_url),
            title,
            id,
        }),
        Event::End(TagEnd::Image) => Event::End(TagEnd::Link),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}
pub fn server(input: &str) -> String {
    SANITIZER.clean(&server_unsanitized(input)).to_string()
}

// A permissive html5ever round trip: what an HTML parser makes of our markup
static BROWSER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    b.tags(
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
            "hr",
        ]
        .into(),
    )
    .generic_attributes(["data-node-hydration"].into())
    .tag_attributes(
        [
            ("a", ["href", "title", "rel"].into()),
            ("ol", ["start"].into()),
            ("code", ["class"].into()),
        ]
        .into(),
    )
    .url_schemes(["http", "https", "mailto"].into())
    .link_rel(None)
    .strip_comments(false);
    b
});
pub fn browser(html: &str) -> String {
    BROWSER.clean(html).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
    Comment(String),
}

pub fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').filter(|e| *e < 12);
        let c = end.and_then(|end| {
            let e = &rest[1..end];
            match e {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => e
                    .strip_prefix("#x")
                    .map(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| e.strip_prefix('#').map(|d| d.parse().ok()))
                    .flatten()
                    .and_then(char::from_u32),
            }
        });
        match (c, end) {
            (Some(c), Some(end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Tokens of well-formed serialized HTML (ours or html5ever's). Voids have no End.
pub fn tokens(html: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").expect("closed comment");
            out.push(Tok::Comment(after[..end].to_string()));
            rest = &after[end + 3..];
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').unwrap();
            let name = after[..end].trim().to_string();
            if name != "br" && name != "hr" {
                out.push(Tok::End(name));
            }
            rest = &after[end + 1..];
        } else if rest.starts_with('<') {
            // quote-aware end
            let b = rest.as_bytes();
            let mut i = 1;
            let mut q = false;
            while i < b.len() {
                if b[i] == b'"' {
                    q = !q
                } else if b[i] == b'>' && !q {
                    break;
                }
                i += 1;
            }
            let inner = rest[1..i].trim_end_matches('/').trim();
            let (name, mut a) =
                inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
            let mut attrs = Vec::new();
            loop {
                a = a.trim_start();
                if a.is_empty() {
                    break;
                }
                let ke = a
                    .find(|c: char| c == '=' || c.is_whitespace())
                    .unwrap_or(a.len());
                let key = a[..ke].to_string();
                a = &a[ke..];
                let value = if let Some(v) = a.strip_prefix("=\"") {
                    let close = v.find('"').unwrap();
                    a = &v[close + 1..];
                    decode(&v[..close])
                } else {
                    String::new()
                };
                attrs.push((key, value));
            }
            attrs.sort();
            out.push(Tok::Start(name.to_string(), attrs));
            rest = &rest[i + 1..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            out.push(Tok::Text(decode(&rest[..end])));
            rest = &rest[end..];
        }
    }
    out
}

/// What the HTML input stream preprocessing does to our text, so only structure is compared
pub fn preprocess(mut t: Vec<Tok>) -> Vec<Tok> {
    for tok in &mut t {
        match tok {
            Tok::Text(s) => {
                *s = s
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\0', "");
            }
            Tok::Start(_, attrs) => {
                for (_, v) in attrs {
                    *v = v
                        .replace("\r\n", "\n")
                        .replace('\r', "\n")
                        .replace('\0', "\u{fffd}");
                }
            }
            _ => {}
        }
    }
    t.retain(|t| !matches!(t, Tok::Text(s) if s.is_empty()));
    t
}

/// Content-model check on our own markup. Returns a complaint.
pub fn model(toks: &[Tok]) -> Option<String> {
    let mut stack: Vec<&str> = Vec::new();
    let blocks = [
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
    ];
    let phrasing_only = [
        "p", "h1", "h2", "h3", "h4", "h5", "h6", "a", "em", "strong", "del",
        "code",
    ];
    let mut max = 0;
    for t in toks {
        match t {
            Tok::Start(name, _) => {
                let n = name.as_str();
                let parent = stack.last().copied();
                let bad = match n {
                    "li" => !matches!(parent, Some("ul" | "ol")),
                    "thead" | "tbody" => parent != Some("table"),
                    "tr" => !matches!(parent, Some("thead" | "tbody")),
                    "th" | "td" => parent != Some("tr"),
                    "a" => stack.contains(&"a"),
                    _ => false,
                } || match parent {
                    Some("ul" | "ol") => n != "li",
                    Some("table") => !matches!(n, "thead" | "tbody"),
                    Some("thead" | "tbody") => n != "tr",
                    Some("tr") => !matches!(n, "th" | "td"),
                    Some("pre") => n != "code",
                    _ => false,
                } || (blocks.contains(&n)
                    && stack.iter().any(|s| phrasing_only.contains(s)));
                if bad {
                    return Some(format!("<{n}> under {stack:?}"));
                }
                if n != "br" && n != "hr" {
                    stack.push(n);
                    max = max.max(stack.len());
                }
            }
            Tok::End(name) => {
                if stack.pop() != Some(name.as_str()) {
                    return Some(format!("misnested </{name}>"));
                }
            }
            Tok::Text(s) => {
                if matches!(
                    stack.last().copied(),
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
                        "text {s:?} directly under {stack:?}"
                    ));
                }
            }
            Tok::Comment(_) => {}
        }
    }
    if !stack.is_empty() {
        return Some(format!("unclosed {stack:?}"));
    }
    if max > 36 {
        return Some(format!("depth {max}"));
    }
    None
}

fn on_site(dest: pulldown_cmark::CowStr<'_>) -> pulldown_cmark::CowStr<'_> {
    if !dest
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("agora://"))
    {
        return dest;
    }
    match dest.parse::<agora_agentkit::links::AgoraLink>() {
        Ok(link) => link.site_path().into(),
        Err(_) => dest,
    }
}
