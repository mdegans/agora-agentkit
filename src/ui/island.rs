//! The data hand-off: the server embeds a root's props in the page, in a
//! `<script type="application/json">` block outside the root's element, and
//! the client mounts the root with what it reads back.
//!
//! The block also carries the agentkit version that drew the server's
//! markup. A client of another version gets [`IslandError::Version`] and
//! must not launch: its components may not draw the same tree, and a
//! hydrating build (dioxus-web's `hydrate` feature) cannot mount fresh at
//! run time instead. The server's markup then stays as the static page it
//! is for readers without JavaScript; one cache-busting reload, to fetch
//! the client the server was built with, is the most a client should try.
//! A fresh mount over the server's markup is the dev build's behaviour,
//! not a fallback of the release build.
//!
//! An HTML parser ends a script block at the first `</script` and reads
//! `<!--` inside one specially, whatever JSON thinks of them, and every
//! string in the props (a title, a name) is someone's to choose. So
//! [`to_script_text`] writes `<`, `>`, `&`, U+2028 and U+2029 as JSON `\u`
//! escapes (hex `003c`, `003e`, `0026`, `2028`, `2029`): no markup is left
//! in the text at all, and U+2028/U+2029 can't end a line should the text
//! ever be read as JavaScript. A JSON parser reads the escapes back as the
//! same characters, so the inverse is a plain parse ([`from_script_text`]).
//!
//! # Reading the block
//!
//! The client selects `script#<id>[type="application/json"]` (the id the
//! server gave [`script_element`], [`DATA_ID`] for the one island there is)
//! and passes its `textContent` to [`from_script_text`]. The server puts
//! the block where no user-supplied HTML precedes it in the page (in the
//! head, or before any rendered content), so nothing earlier in the
//! document can open an element that swallows or shadows it.

use std::{borrow::Cow, fmt::Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned, de::IgnoredAny};

/// The id of the element a page provides for the root (the client's mount
/// point, `dioxus::web::Config::rootname`)
pub const ROOT_ID: &str = "agora-app";

/// The id of the page's data block for [`ROOT_ID`]'s island
pub const DATA_ID: &str = "agora-app-data";

/// This crate's version, which drew the markup
pub const AGENTKIT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the data block holds: a root's props and who drew them
#[derive(Serialize, Deserialize)]
struct Island<'a, P> {
    #[serde(borrow)]
    agentkit: Cow<'a, str>,
    props: P,
}

/// Why a client can't hydrate from a page's data block
#[derive(Debug, thiserror::Error)]
pub enum IslandError {
    /// Not the JSON of a data block of these props
    #[error("the page's data couldn't be read: {0}")]
    Json(#[from] serde_json::Error),
    /// Drawn by another agentkit, whose components may draw another tree.
    /// Checked before the props are read, so a page whose props changed
    /// shape says this rather than [`IslandError::Json`]
    #[error(
        "the page was drawn with agora-agentkit {page}, this client has {client}"
    )]
    Version { page: String, client: &'static str },
}

/// A page's data as an anonymous reader reads it: what [`to_script_text`],
/// [`script_element`] and the pre-render embed or draw. A page's data is
/// that reader's view, enforced where it is rendered: whatever the server
/// built a page from (a signed read, its own rows), the reader-specific
/// fields never reach a public page. Only these render paths enforce it:
/// serialising a page type yourself (`serde_json::to_string(&page)`) does
/// not, and calling `public()` first is then the caller's responsibility
pub trait Public {
    /// `self`, with everything that depends on who reads it cleared.
    /// Idempotent
    fn public(self) -> Self;
}

/// SPIKE: a page with no data (the router's pages without one)
impl<P: Public> Public for Option<P> {
    fn public(self) -> Self {
        self.map(Public::public)
    }
}

/// `props`' public form ([`Public`]) as the text of the data block: JSON
/// with no `<`, `>`, `&`, U+2028 or U+2029 left in it
pub fn to_script_text<P: Serialize + Public + Clone>(
    props: &P,
) -> Result<String, serde_json::Error> {
    encode(&props.clone().public())
}

/// Any value as the text of a data block, escaped as [`to_script_text`]'s
pub(crate) fn encode<P: Serialize>(
    props: &P,
) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(&Island {
        agentkit: Cow::Borrowed(AGENTKIT_VERSION),
        props,
    })?;
    Ok(escape(&json))
}

/// The whole data block, `<script type="application/json" id="{id}">…</script>`,
/// to put in the page as is, outside the root's element (see the module
/// docs for where). `id` is the page's own constant, like [`DATA_ID`]:
/// lowercase letters, digits and hyphens
pub fn script_element<P: Serialize + Public + Clone>(
    id: &'static str,
    props: &P,
) -> Result<String, serde_json::Error> {
    assert!(
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || b == b'-'),
        "a data block's id is lowercase letters, digits and hyphens: {id:?}"
    );
    let text = to_script_text(props)?;
    Ok(format!(
        r#"<script type="application/json" id="{id}">{text}</script>"#
    ))
}

/// The props back from the data block's text (its `textContent`), if this
/// agentkit drew them. The version is read first, the props after
pub fn from_script_text<P: DeserializeOwned>(
    text: &str,
) -> Result<P, IslandError> {
    let stamp: Island<'_, IgnoredAny> = serde_json::from_str(text)?;
    if stamp.agentkit != AGENTKIT_VERSION {
        return Err(IslandError::Version {
            page: stamp.agentkit.into_owned(),
            client: AGENTKIT_VERSION,
        });
    }
    let island: Island<'_, P> = serde_json::from_str(text)?;
    Ok(island.props)
}

/// JSON's text with the five characters written as `\u` escapes.
/// serde_json writes none of them outside a string, and inside one a `\u`
/// escape reads back as the character
fn escape(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                write!(out, "\\u{:04x}", c as u32)
                    .expect("a String takes writes");
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
