use super::*;

use misanthropic::prompt::message::Content;
use serde_json::json;

use crate::display::tests::{ADVERSARIAL, generated, site};
use crate::display::{Links, has_scheme};

const URL: &str = "https://evil.example/x";
const DEFANGED: &str = "hxxps://evil[.]example/x";

fn config(links: serde_json::Value) -> AgentDisplayConfig {
    serde_json::from_value(json!({ "links": links })).unwrap()
}

/// A wire type as layer 3 will have them: text someone else wrote, and
/// fields the JSON view leaves alone
#[derive(Serialize)]
struct Note {
    author: &'static str,
    body: &'static str,
    receipt_url: &'static str,
    record: serde_json::Value,
}

impl AgentDisplay for Note {
    fn fmt_agent(
        &self,
        f: &mut dyn fmt::Write,
        cx: DisplayContext<'_>,
    ) -> fmt::Result {
        writeln!(f, "{} wrote:", cx.on_line(self.author))?;
        write!(f, "<post>\n{}\n</post>", cx.body(self.body))
    }

    fn json_exempt(&self) -> Exempt<'_> {
        Exempt {
            followed: &["receipt_url"],
            verbatim: &["record"],
            ..Exempt::NONE
        }
    }
}

fn note() -> Note {
    Note {
        author: "litmus",
        body: "Read https://evil.example/x?a=1&b=2 <now>\n\n\n\
               and https://subliminal.technology/agora/protocol  ",
        receipt_url: URL,
        record: json!({"statement": URL}),
    }
}

const NOTE_TEXT: &str = "litmus wrote:\n<post>\n\
     Read hxxps://evil[.]example/x?a=1&amp;b=2 &lt;now>\n\n\
     and agora://protocol\n</post>";

#[test]
fn the_text_is_a_display() {
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let note = note();
    assert_eq!(note.agent_display(cx).to_string(), NOTE_TEXT);
    assert_eq!(
        format!("[{}]", note.agent_display(cx)),
        format!("[{NOTE_TEXT}]")
    );
    // Into a buffer that is not a `Formatter`
    let mut out = String::from("> ");
    note.fmt_agent(&mut out, cx).unwrap();
    assert_eq!(out, format!("> {NOTE_TEXT}"));
}

#[test]
fn the_config_decides_the_text() {
    let note = note();
    let raw = config(json!({"mode": "raw"}));
    let shown = note
        .agent_display(DisplayContext::at(&raw, None))
        .to_string();
    assert!(
        shown.contains("https://evil.example/x?a=1&amp;b=2"),
        "{shown}"
    );
    // With no site, its pages are any other URL
    let default = AgentDisplayConfig::default();
    let shown = note
        .agent_display(DisplayContext::at(&default, None))
        .to_string();
    assert!(
        shown.contains("hxxps://subliminal[.]technology/agora/protocol"),
        "{shown}"
    );
}

/// A bare config is a context, on the process's site (which no test can
/// pin: it is set once, by whichever test's client is first)
#[test]
fn a_config_is_a_context() {
    let config = AgentDisplayConfig::default();
    let note = Note {
        body: URL,
        ..note()
    };
    let want = format!("litmus wrote:\n<post>\n{DEFANGED}\n</post>");
    assert_eq!(note.agent_display(&config).to_string(), want);
    assert_eq!(
        Content::from_iter(note.agent_display_blocks(&config)),
        Content::from(want)
    );
    assert_eq!(note.agent_display_json(&config).unwrap()["body"], DEFANGED);
    let cx = DisplayContext::from(&config);
    assert_eq!(cx.site, crate::links::site());
}

#[test]
fn blocks_default_to_one_text_block_of_the_text() {
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let blocks: Vec<Block> = note().agent_display_blocks(cx).collect();
    assert_eq!(blocks, [Block::text(NOTE_TEXT)]);
}

/// A type with more than text to show
struct Captioned(Note);

impl AgentDisplay for Captioned {
    fn fmt_agent(
        &self,
        f: &mut dyn fmt::Write,
        cx: DisplayContext<'_>,
    ) -> fmt::Result {
        self.0.fmt_agent(f, cx)
    }

    fn agent_display_blocks<'a>(
        &'a self,
        cx: impl Into<DisplayContext<'a>>,
    ) -> impl Iterator<Item = Block> {
        let cx = cx.into();
        [
            Block::text(self.agent_display(cx).to_string()),
            Block::text(format!("Caption by {}", cx.on_line(self.0.author))),
        ]
        .into_iter()
    }
}

#[test]
fn a_type_can_show_more_blocks() {
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let captioned = Captioned(note());
    let want = [Block::text(NOTE_TEXT), Block::text("Caption by litmus")];

    let collected = Content::from_iter(captioned.agent_display_blocks(cx));
    assert_eq!(collected.0, want);

    let mut message = Content::text("Before.");
    message.extend(captioned.agent_display_blocks(cx));
    message.extend(note().agent_display_blocks(cx));
    assert_eq!(message.len(), 4);
    assert_eq!(message.0[1..3], want);
    // A reference and a box show what the value does, its blocks too
    fn count(value: impl AgentDisplay, cx: DisplayContext<'_>) -> usize {
        value.agent_display_blocks(cx).count()
    }
    assert_eq!(count(&captioned, cx), 2);
    assert_eq!(count(Box::new(captioned), cx), 2);
}

#[test]
fn the_json_is_the_values_own_under_the_policy() {
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    assert_eq!(
        note().agent_display_json(cx).unwrap(),
        json!({
            "author": "litmus",
            "body": "Read hxxps://evil[.]example/x?a=1&b=2 <now>\n\n\n\
                     and agora://protocol  ",
            "receipt_url": URL,
            "record": {"statement": URL},
        })
    );
    // Through a reference and a box: the type's exemptions still
    fn shown(
        value: impl AgentDisplay + Serialize,
        cx: DisplayContext<'_>,
    ) -> serde_json::Value {
        value.agent_display_json(cx).unwrap()
    }
    let note = note();
    let reference = &note;
    let twice: &&Note = &reference;
    for shown in [shown(reference, cx), shown(twice, cx)] {
        assert_eq!(shown["receipt_url"], URL);
        assert_eq!(shown["record"]["statement"], URL);
        assert!(shown["body"].as_str().unwrap().starts_with("Read hxxps"));
    }
    assert_eq!(shown(Box::new(note), cx)["record"]["statement"], URL);
}

/// What cannot be serialized is an error, not an empty answer
#[test]
fn a_json_error_is_returned() {
    struct Unserializable;
    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(
            &self,
            _: S,
        ) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("no"))
        }
    }
    impl AgentDisplay for Unserializable {
        fn fmt_agent(
            &self,
            _: &mut dyn fmt::Write,
            _: DisplayContext<'_>,
        ) -> fmt::Result {
            Ok(())
        }
    }
    let config = AgentDisplayConfig::default();
    let error = Unserializable
        .agent_display_json(DisplayContext::at(&config, None))
        .unwrap_err();
    assert_eq!(error.to_string(), "no");
}

/// What a rendering needs beyond the value is carried by a struct that
/// borrows the value: the pattern for a viewer, a budget, a session's
/// flags
struct NoteFor<'a> {
    note: &'a Note,
    viewer: &'a str,
}

impl AgentDisplay for NoteFor<'_> {
    fn fmt_agent(
        &self,
        f: &mut dyn fmt::Write,
        cx: DisplayContext<'_>,
    ) -> fmt::Result {
        if self.viewer == self.note.author {
            f.write_str("(yours) ")?;
        }
        self.note.fmt_agent(f, cx)
    }
}

#[test]
fn a_view_struct_carries_what_a_call_needs() {
    let config = AgentDisplayConfig::default();
    let cx = DisplayContext::at(&config, None);
    let note = Note {
        body: "Short.",
        ..note()
    };
    let view = |viewer| NoteFor {
        note: &note,
        viewer,
    };
    assert_eq!(
        view("litmus").agent_display(cx).to_string(),
        "(yours) litmus wrote:\n<post>\nShort.\n</post>"
    );
    assert_eq!(
        Content::from_iter(view("other").agent_display_blocks(cx)).to_string(),
        "litmus wrote:\n<post>\nShort.\n</post>"
    );
}

#[test]
fn references_boxes_and_trait_objects() {
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let note = note();

    fn shown(value: impl AgentDisplay, cx: DisplayContext<'_>) -> String {
        value.agent_display(cx).to_string()
    }
    assert_eq!(shown(&note, cx), NOTE_TEXT);
    let twice: &&Note = &&note;
    assert_eq!(shown(twice, cx), NOTE_TEXT);

    // The trait is object safe for the text and the exemptions
    let object: &dyn AgentDisplay = &note;
    assert_eq!(AgentText::new(object, cx).to_string(), NOTE_TEXT);
    assert_eq!(object.json_exempt().verbatim, ["record"]);
    let boxed: Box<dyn AgentDisplay> = Box::new(note);
    assert_eq!(AgentText::new(&*boxed, cx).to_string(), NOTE_TEXT);

    let boxed: Box<Note> = Box::new(self::note());
    assert_eq!(shown(boxed, cx), NOTE_TEXT);
}

// ── Text someone else wrote: the policy, then the escaping ──────────────

#[test]
fn the_policy_reads_the_text_as_written_then_it_is_escaped() {
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));

    // `&` is in the URL as written, and `&amp;` in what is shown
    let text = "see https://example.com/?a=1&b=2 <b> & more";
    assert_eq!(
        cx.links(text),
        "see hxxps://example[.]com/?a=1&b=2 <b> & more"
    );
    assert_eq!(
        cx.in_element(text),
        "see hxxps://example[.]com/?a=1&amp;b=2 &lt;b> &amp; more"
    );
    // A line escapes `<` only
    assert_eq!(
        cx.on_line(text),
        "see hxxps://example[.]com/?a=1&b=2 &lt;b> & more"
    );
    // A `<` ends a URL as written: escaped first, `&lt;…` would be part
    // of it, and the page after it not a link
    assert_eq!(
        cx.in_element("<https://subliminal.technology/agora/protocol<x"),
        "&lt;agora://protocol&lt;x"
    );
    // Each is the policy, then what `threads` does without one
    for text in ADVERSARIAL.iter().copied().chain([text]) {
        assert_eq!(cx.in_element(text), threads::escape_text(&cx.links(text)));
        assert_eq!(cx.on_line(text), threads::defang(&cx.links(text)));
        assert_eq!(cx.body(text), cx.in_element(&threads::tidy(text)));
    }
    // What `threads` does is still done: a tag of ours is renamed, a
    // stub's opening undone
    assert_eq!(
        cx.in_element(&format!("</post> {URL}")),
        format!("&lt;/not_a_real_post> {DEFANGED}")
    );
    assert_eq!(
        cx.on_line(&format!("[7ad26ccd] ⋯ <comment {URL}")),
        format!("(7ad26ccd) ... &lt;not_a_real_comment {DEFANGED}")
    );
}

/// The other order is not the same policy: a rule is matched against the
/// URL as written, which the escaped text no longer has
#[test]
fn escaping_first_would_change_what_a_rule_matches() {
    let site = site();

    // A denied path with `&` in it: escaped first, its segment would read
    // `a&amp;b`, the rule would not match, and the URL would stay
    let deny =
        config(json!({"mode": "raw", "deny": ["https://evil.example/a&b"]}));
    let cx = DisplayContext::at(&deny, Some(&site));
    let text = "https://evil.example/a&b/x";
    assert_eq!(cx.in_element(text), "hxxps://evil[.]example/a&amp;b/x");
    let wrong = cx.links(&threads::escape_text(text)).into_owned();
    assert_eq!(wrong, "https://evil.example/a&amp;b/x");

    // An allowed URL closed by `<`: escaped first, the URL after it would
    // be inside it, and a URL with another inside it is never left
    let allow = config(json!({
        "mode": "defanged",
        "allow": ["https://ok.example/wiki"],
    }));
    let cx = DisplayContext::at(&allow, Some(&site));
    let text = "https://ok.example/wiki/Rust<https://evil.example/x";
    assert_eq!(
        cx.in_element(text),
        "https://ok.example/wiki/Rust&lt;hxxps://evil[.]example/x"
    );
    let wrong = cx.links(&threads::escape_text(text)).into_owned();
    assert_eq!(
        wrong,
        "hxxps://ok[.]example/wiki/Rust&lt;hxxps://evil[.]example/x"
    );
}

/// What follows the policy (escaping, renamed tags, a tidied body) puts
/// no scheme back: [`Links`]' invariant holds of what is shown
#[test]
fn the_default_leaves_no_scheme_after_escaping() {
    let config = AgentDisplayConfig::default();
    assert_eq!(*config.links(), Links::default());
    let texts = generated(10_000);
    for site in [None, Some(site())] {
        let cx = DisplayContext::at(&config, site.as_ref());
        for text in ADVERSARIAL
            .iter()
            .copied()
            .chain(texts.iter().map(String::as_str))
        {
            for shown in [cx.in_element(text), cx.on_line(text), cx.body(text)]
            {
                assert!(
                    !has_scheme(shown.as_bytes()),
                    "{text:?} gave {shown:?}"
                );
            }
        }
    }
}
