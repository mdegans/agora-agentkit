//! The data hand-off: the server embeds a root's props in the page, in a
//! `<script type="application/json">` block outside the root's element, and
//! the client mounts the root with what it reads back.
//!
//! The block holds an [`Island`]: the props, and the agentkit version that
//! drew the server's markup. A client of another version is told so
//! ([`IslandError::Version`]) and should mount fresh rather than hydrate:
//! its components may not draw the same tree.
//!
//! An HTML parser ends a script block at the first `</script` and reads
//! `<!--` inside one specially, whatever JSON thinks of them, and every
//! string in the props (a title, a name) is someone's to choose. So
//! [`to_script_text`] writes `<`, `>` and `&` as JSON's `<`, `>`
//! and `&`, and U+2028 and U+2029 as ` ` and ` ` (line ends
//! in JavaScript, should the text ever be read as script). No markup is
//! left in the text at all. A JSON parser reads the escapes back as the
//! same characters, so the inverse is a plain parse ([`from_script_text`]).

use std::borrow::Cow;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The id of the element a page provides for the root (the client's mount
/// point, `dioxus::web::Config::rootname`)
pub const ROOT_ID: &str = "agora-app";

/// The id of the page's `<script type="application/json">` holding the
/// [`Island`]
pub const DATA_ID: &str = "agora-app-data";

/// This crate's version, which drew the markup
pub const AGENTKIT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the data block holds: a root's props and who drew them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Island<'a, P> {
    /// The agentkit version of the server's pre-render
    pub agentkit: Cow<'a, str>,
    /// The root's props
    pub props: P,
}

/// Why a client can't hydrate from a page's data block
#[derive(Debug, thiserror::Error)]
pub enum IslandError {
    /// Not the JSON of an [`Island`] of these props
    #[error("the page's data couldn't be read: {0}")]
    Json(#[from] serde_json::Error),
    /// Drawn by another agentkit, whose components may draw another tree
    #[error(
        "the page was drawn with agora-agentkit {page}, this client has {client}"
    )]
    Version { page: String, client: &'static str },
}

/// `props` as the text of the data block: JSON with no `<`, `>`, `&`,
/// U+2028 or U+2029 left in it
pub fn to_script_text<P: Serialize>(
    props: &P,
) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(&Island {
        agentkit: Cow::Borrowed(AGENTKIT_VERSION),
        props,
    })?;
    Ok(escape(&json))
}

/// The whole data block, `<script type="application/json" id="agora-app-data">…</script>`,
/// to put in the page as is, outside [`ROOT_ID`]'s element
pub fn script_element<P: Serialize>(
    props: &P,
) -> Result<String, serde_json::Error> {
    let text = to_script_text(props)?;
    Ok(format!(
        r#"<script type="application/json" id="{DATA_ID}">{text}</script>"#
    ))
}

/// The props back from the data block's text (its `textContent`), if this
/// agentkit drew them
pub fn from_script_text<P: DeserializeOwned>(
    text: &str,
) -> Result<P, IslandError> {
    let island: Island<'static, P> = serde_json::from_str(text)?;
    if island.agentkit != AGENTKIT_VERSION {
        return Err(IslandError::Version {
            page: island.agentkit.into_owned(),
            client: AGENTKIT_VERSION,
        });
    }
    Ok(island.props)
}

/// JSON's text with the five characters replaced by their `\u` escapes.
/// serde_json writes none of them outside a string, and inside one a `\u`
/// escape reads back as the character
fn escape(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strings an attacker would title a post with
    const HOSTILE: &[&str] = &[
        "</script><script>alert(1)</script>",
        "</SCRIPT >",
        "<!-- <script>",
        "]]><![CDATA[",
        "&lt;b&gt; &amp; &#60;",
        "line\u{2028}sep\u{2029}para",
        r#""quoted" 'single' \backslash < literal"#,
        "<img src=x onerror=alert(1)>",
        "\0 nul and \u{1b} escape",
        "emoji 🦀 and 𝕏 outside the BMP",
    ];

    #[test]
    fn the_text_has_no_markup_and_reads_back() {
        for &hostile in HOSTILE {
            let props = vec![hostile.to_string()];
            let text = to_script_text(&props).unwrap();
            for bad in ['<', '>', '&', '\u{2028}', '\u{2029}'] {
                assert!(!text.contains(bad), "{bad:?} in {text}");
            }
            let back: Vec<String> = from_script_text(&text).unwrap();
            assert_eq!(back, props, "{text}");
        }
    }

    #[test]
    fn the_element_holds_only_the_escaped_text() {
        let props = HOSTILE.join("");
        let element = script_element(&props).unwrap();
        let open = r#"<script type="application/json" id="agora-app-data">"#;
        let inner = element
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix("</script>"))
            .unwrap();
        assert!(!inner.contains('<'), "{inner}");
        assert_eq!(from_script_text::<String>(inner).unwrap(), props);
    }

    #[test]
    fn a_very_long_string_round_trips() {
        let props = "</script>\u{2028}&".repeat(100_000);
        let text = to_script_text(&props).unwrap();
        assert!(!text.contains('<'));
        assert_eq!(from_script_text::<String>(&text).unwrap(), props);
    }

    /// No Rust string holds a lone surrogate, so the server never writes
    /// one; a page that does is refused, not read as something else
    #[test]
    fn a_lone_surrogate_is_refused() {
        for escape in [r"\ud800", r"\udfff", r"\ud800A"] {
            let text = format!(
                r#"{{"agentkit":"{AGENTKIT_VERSION}","props":"{escape}"}}"#
            );
            assert!(
                matches!(
                    from_script_text::<String>(&text),
                    Err(IslandError::Json(_))
                ),
                "{text}"
            );
        }
        // A pair is one character
        let text =
            format!(r#"{{"agentkit":"{AGENTKIT_VERSION}","props":"🦀"}}"#);
        assert_eq!(from_script_text::<String>(&text).unwrap(), "🦀");
    }

    #[test]
    fn another_agentkits_page_is_refused() {
        let text = r#"{"agentkit":"0.0.1","props":"x"}"#;
        match from_script_text::<String>(text) {
            Err(IslandError::Version { page, client }) => {
                assert_eq!(page, "0.0.1");
                assert_eq!(client, AGENTKIT_VERSION);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_shape_is_props_and_version() {
        let text = to_script_text(&1u8).unwrap();
        assert_eq!(
            text,
            format!(r#"{{"agentkit":"{AGENTKIT_VERSION}","props":1}}"#)
        );
    }
}
