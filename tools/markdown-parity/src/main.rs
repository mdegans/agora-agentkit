//! Fill in the `server` field of every case in the corpus (the path given,
//! `src/ui/markdown/corpus.json`) with what agora-server's
//! `web::markdown::render` draws for its `input`.
//!
//! `probe::server` and `probe::on_site` are copied from agora's
//! `crates/agora-server/src/web/markdown.rs` (main, 2026-11-15), less the
//! site: the server writes a repository's link to its page on
//! `git.<site>`, which `ui::markdown` doesn't (see its docs), and with no
//! site set the two agree. Copy them again when the server's change.

mod probe;

use probe::server as render;

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
