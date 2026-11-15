//! Fill in the `server` field of every case in the corpus (the path given,
//! `src/ui/markdown/corpus.json`) with what agora-server's
//! `web::markdown::render` draws for its `input`.
//!
//! `render` and `on_site` below are copied from agora's
//! `crates/agora-server/src/web/markdown.rs` (main, 2026-11-15), less the
//! site: the server writes a repository's link to its page on
//! `git.<site>`, which `ui::markdown` doesn't (see its docs), and with no
//! site set the two agree. Copy them again when the server's change.

use std::sync::LazyLock;

use agora_agentkit::links::AgoraLink;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut builder = ammonia::Builder::default();
    builder
        .url_schemes(["http", "https", "mailto"].into())
        .link_rel(Some("noopener noreferrer nofollow ugc"));
    builder
});

fn render(input: &str) -> String {
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
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    SANITIZER.clean(&html_output).to_string()
}

fn on_site(dest: CowStr<'_>) -> CowStr<'_> {
    if !dest
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("agora://"))
    {
        return dest;
    }
    match dest.parse::<AgoraLink>() {
        Ok(link) => link.site_path().into(),
        Err(_) => dest,
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: markdown-parity <corpus.json>");
    let text = std::fs::read_to_string(&path).expect("the corpus");
    let mut corpus: Vec<serde_json::Value> =
        serde_json::from_str(&text).expect("a JSON list");
    for case in &mut corpus {
        let input = case["input"].as_str().expect("an input").to_string();
        case["server"] = render(&input).into();
    }
    let mut out = serde_json::to_string_pretty(&corpus).unwrap();
    out.push('\n');
    std::fs::write(&path, out).expect("written");
}
