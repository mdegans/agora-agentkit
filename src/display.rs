//! How Agora's content is shown to a reader: the options a view is built
//! with, the link policy ([`Links`]) applied to text others wrote, and the
//! agent's view of a wire type ([`AgentDisplay`]: text and content blocks;
//! [`AgentJson`]: JSON). An operator's config for many agents (a default, named profiles,
//! each agent's or model's choice) is [`AgentDisplayProfiles`].
//!
//! A URL in a feed is a fetch target for any reading agent whose fetch tool
//! takes URLs it has seen (agora#332), so by default every off-site URL is
//! defanged (`hxxps://example[.]com/x`): legible as a citation, not
//! fetchable as written. A page URL of Agora's own site is written as its
//! `agora://` link ([`AgoraLink`]), which a tool reads.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use url::{Host, Position, Url};

use crate::links::AgoraLink;
use crate::threads;

mod agent;
mod json;
#[cfg(test)]
pub(crate) mod oracle;
mod profiles;

pub use agent::{AgentDisplay, AgentText, DisplayContext};
pub use json::{AgentJson, Exempt, json_view};
pub use profiles::{
    AgentDisplayProfiles, DisplaySelection, ProfileName, ProfileNameError,
    ResolvedDisplay, SelectedBy, UnknownProfile,
};

/// Options common to every view, an agent's or a human's.
///
/// An unknown key is refused: a typo must not leave a policy at its default
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    /// What becomes of URLs in text others wrote
    #[serde(default)]
    pub links: Links,
}

/// Options of an agent's view: [`DisplayConfig`]'s keys, at the same level.
///
/// An unknown key is refused here too
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
pub struct AgentDisplayConfig {
    // `flatten` and `deny_unknown_fields` do not combine, so the keys that
    // are not this struct's own are handed to `DisplayConfig`, which refuses
    // the ones it does not know
    #[serde(flatten, deserialize_with = "strict_common")]
    pub common: DisplayConfig,
}

impl AgentDisplayConfig {
    pub fn links(&self) -> &Links {
        &self.common.links
    }
}

impl AsRef<DisplayConfig> for AgentDisplayConfig {
    fn as_ref(&self) -> &DisplayConfig {
        &self.common
    }
}

impl AsRef<DisplayConfig> for DisplayConfig {
    fn as_ref(&self) -> &DisplayConfig {
        self
    }
}

impl From<DisplayConfig> for AgentDisplayConfig {
    fn from(common: DisplayConfig) -> Self {
        Self { common }
    }
}

/// Every key the flattening struct did not take, read as a [`DisplayConfig`]
fn strict_common<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<DisplayConfig, D::Error> {
    let rest = serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
    DisplayConfig::deserialize(serde_json::Value::Object(rest))
        .map_err(serde::de::Error::custom)
}

/// What becomes of URLs in text others wrote.
///
/// In both modes a page URL of Agora's site is written as its `agora://`
/// link, and a page of the site with no link is treated as any other URL.
///
/// A URL starts at `http` or `https`, a `:` and any run of `/` and `\`
/// (`https://x`, `https:/x`, `https:x`, `https:\\x`), in any letter case,
/// with tabs and line breaks anywhere in it, which URL parsers drop:
/// whatever a WHATWG parser reads as `http` or `https` with a host. Here
/// (where a URL starts, not where it ends) a line break is any
/// [`threads::is_line_break`], with the whitespace
/// before it, since [`threads::tidy`] makes `\n` of those after the
/// policy has run. So `https:` at the end of a line is taken for a URL
/// with the next line's first word, and its scheme defanged; before a
/// space and a word it is prose.
///
/// What holds of the result, whatever the text:
///
/// - **`defanged` with no `allow` rule** (the default): no `http` or
///   `https` URL is left in it. Nowhere in it does a URL parser, given
///   the text from there to the next space with tabs and line breaks
///   dropped, read one with a host that has a letter or a digit.
/// - **`defanged` with `allow` rules**: an `http` or `https` URL left in
///   it is one a rule allows, under every reading below.
/// - **`raw` with `deny` rules**: no `http` or `https` URL left in it is
///   one a rule denies, under every reading below.
///
/// **A URL's readings.** Whitespace of any kind, a tab or a line break
/// ends a URL. Where before that it ends depends on who reads it, and a
/// rule that passed the URL up to a `'` would say nothing of
/// `https://allowed.example'@evil.example/`, which a parser reads whole.
/// So a URL is left as written only if every reading of it passes: the
/// one that ends as prose does (before any of ``<>"'`()[]{}|``), and each
/// that ends at a later one of those or at the whitespace. Sentence
/// punctuation (`.,;:!?*_`), brackets and quotes at the end of a reading
/// are not part of it. Each reading is checked alone: under `allow` each
/// must be allowed, by any rule. A later reading whose host ends in a
/// label with punctuation no domain name has, as written in ASCII
/// (`example.org),next`, `example.org">t<`), names nothing to fetch and
/// is not checked.
///
/// A URL is never left as written on the strength of a rule it cannot be
/// checked against. With rules, each of these is defanged, allowed or
/// not denied as it may be: a URL with another inside it, before its
/// first bracket or quote (the one inside is judged on its own, as every
/// URL start is); one with a reading that does not parse, or that has a
/// backslash or a control character; one whose scheme is not written
/// `http://` or `https://`; one longer than [`MAX_URL_BYTES`] to the
/// next space; and one with more than [`MAX_URL_ENDS`] readings.
///
/// **Not covered**, in any mode:
///
/// - What no URL parser reads as `http` or `https`. Other schemes
///   (`ftp://`, `ws://`, `file://`), a protocol-relative `//host/path`, a
///   bare `host.tld/path` other than `www.`, a scheme in full-width or
///   look-alike letters, HTML entities and a percent-encoded scheme are
///   left as written; the server's scanner left them too.
/// - A reader that deletes the whitespace or line breaks inside a URL
///   (`https://allowed.example\n.evil.com`): rules judge a URL to its
///   first whitespace (the Steward's call, 2026-11-16: what a model can
///   hand a fetch tool is what is checked).
/// - A cut made after the policy can make a different URL of one it
///   judged: `https://denied.example.ok.com/` cut after `denied.example`.
/// - A deny rule whose path ends in sentence punctuation (`/a.`) does not
///   match where that character is read as the sentence's.
///
/// ```toml
/// links = { mode = "defanged", allow = ["https://en.wikipedia.org/wiki"] }
/// links = { mode = "raw", deny = ["https://*.example.org"] }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Links {
    /// Every off-site URL is defanged but those an `allow` rule matches
    /// ([`LinkRule::allows`]). A schemeless `www.` host matches none.
    Defanged {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<LinkRule>,
    },
    /// Every URL is left as written but those a `deny` rule matches
    /// ([`LinkRule::denies`]), which are defanged. A schemeless `www.` host
    /// is read as `http://`.
    Raw {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deny: Vec<LinkRule>,
    },
}

/// Everything defanged: what agents were shown before this was an option
impl Default for Links {
    fn default() -> Self {
        Self::Defanged { allow: Vec::new() }
    }
}

/// The longest URL read as a link or checked against a [`LinkRule`],
/// from its scheme to the next space: a longer one matches no rule. It
/// bounds the work per URL
pub const MAX_URL_BYTES: usize = 2_048;

/// The most readings of one URL that are checked against the rules (see
/// [`Links`]): one with more is not left as written. With
/// [`MAX_URL_BYTES`] it bounds the work per URL
pub const MAX_URL_ENDS: usize = 16;

/// A URL start: a scheme, or a schemeless `www.` host
#[derive(Debug, Clone, Copy)]
struct Start {
    /// The scheme with its `:` and slashes; 0 for a `www.` host
    len: usize,
    /// Written `http://` or `https://` and no other way
    plain: bool,
}

impl Start {
    const WWW: Self = Self {
        len: 0,
        plain: false,
    };
}

impl Links {
    /// `text` under this policy, with page URLs of `site` as their links.
    /// Borrowed when nothing changes.
    ///
    /// A scheme's `http` becomes `hxxp` (lower case), wherever it is
    /// written (`xhttp://`, `a:https://`), and the dots of the authority
    /// `[.]`; a schemeless `www.` host at the start of a word gets the
    /// dots. Everything else is copied: the `:` and slashes as written,
    /// the path and query.
    ///
    /// Time is linear in `text`: one pass, and each URL start reads at
    /// most [`MAX_URL_BYTES`] ahead and is parsed at most once for the
    /// site and [`MAX_URL_ENDS`] + 1 times for the rules. No URL is
    /// parsed when there is neither a site nor a rule.
    pub fn rewrite<'a>(
        &self,
        text: &'a str,
        site: Option<&Url>,
    ) -> Cow<'a, str> {
        self.rewrite_noting(text, site).0
    }

    /// [`rewrite`](Self::rewrite), and whether it defanged anything (a
    /// site page written as its link is not)
    pub fn rewrite_noting<'a>(
        &self,
        text: &'a str,
        site: Option<&Url>,
    ) -> (Cow<'a, str>, bool) {
        let mut defanged = false;
        let bytes = text.as_bytes();
        let mut out: Option<String> = None;
        let mut i = 0;
        let mut copied = 0; // everything before this offset is already in `out`

        while i < bytes.len() {
            let start = if let Some(scheme) = scheme_at(text, i) {
                scheme
            } else if www_at(bytes, i) {
                Start::WWW
            } else {
                i += 1;
                continue;
            };
            let authority = i + start.len;

            // This site's page: its link, which an agent can follow
            if let Some(site) = site
                .filter(|s| start.plain && on_site_host(bytes, authority, s))
                && let Some(end) = url_end(text, i, authority)
                && let Ok(link) = AgoraLink::parse_at(&text[i..end], Some(site))
                    .map(|link| link.to_string())
                // (links percent-encode a URL they carry; were one ever
                // written whole, the page would be any other URL)
                && !has_scheme(&link)
            {
                let buf = out.get_or_insert_with(|| {
                    String::with_capacity(text.len() + 16)
                });
                buf.push_str(&text[copied..i]);
                buf.push_str(&link);
                copied = end;
                i = end;
                continue;
            }

            if let Some(after) = self.leaves(text, i, start) {
                i = after;
                continue;
            }

            defanged = true;
            let buf = out
                .get_or_insert_with(|| String::with_capacity(text.len() + 16));
            buf.push_str(&text[copied..i]);
            // `hxxp`, and what was written between and after its letters
            buf.extend(text[i..authority].chars().map(|c| {
                match c.to_ascii_lowercase() {
                    't' => 'x',
                    other => other,
                }
            }));

            // The dots of the rest of the authority (for a `www.` start,
            // its own included)
            let j = authority_end(text, authority);
            for b in &bytes[authority..j] {
                if *b == b'.' {
                    buf.push_str("[.]");
                } else {
                    buf.push(*b as char);
                }
            }
            copied = j;
            i = j.max(i + 1);
        }

        let out = match out {
            Some(mut buf) => {
                buf.push_str(&text[copied..]);
                Cow::Owned(buf)
            }
            None => Cow::Borrowed(text),
        };
        (out, defanged)
    }

    /// Where to go on from when the URL written at `at` is left as
    /// written: its end as prose, so nothing in it is rewritten after it
    /// was checked, and a URL start after that is judged on its own
    fn leaves(&self, text: &str, at: usize, start: Start) -> Option<usize> {
        match self {
            Self::Defanged { allow } => {
                if allow.is_empty() || start.len == 0 {
                    return None;
                }
                every_reading(text, at, start, |url| {
                    allow.iter().any(|r| r.allows(url))
                })
            }
            // Nothing to check against, and nothing is ever defanged
            Self::Raw { deny } if deny.is_empty() => {
                Some(at + start.len.max(1))
            }
            Self::Raw { deny } => every_reading(text, at, start, |url| {
                !deny.iter().any(|r| r.denies(url))
            }),
        }
    }
}

/// The end as prose of the URL written at `at`, when every reading of it
/// (see [`Links`]) parses and passes (`http://` put before a schemeless
/// one). `None` too when it has another URL start inside it, is too long
/// or has too many readings
fn every_reading(
    text: &str,
    at: usize,
    start: Start,
    pass: impl Fn(&Url) -> bool,
) -> Option<usize> {
    if start.len > 0 && !start.plain {
        return None;
    }
    let bytes = text.as_bytes();
    let from = at + start.len;
    let passes = |reading: &str| {
        if reading.bytes().any(|b| b == b'\\' || b.is_ascii_control()) {
            return false;
        }
        let url = if start.len == 0 {
            Url::parse(&["http://", reading].concat())
        } else {
            Url::parse(reading)
        };
        url.is_ok_and(|url| pass(&url))
    };
    // A reading and, when the dots after it make its last path segment
    // `.` or `..`, the reading with them: `/wiki/x/..` at a sentence's
    // end is `/wiki/x/` without its dots and `/wiki/` with them
    let passes_dotted = |read: &str, rest: &str| {
        let dots = rest.bytes().take_while(|b| *b == b'.').count().min(2);
        // (`%2e%2e` is the longest spelling of one)
        let tail = &read.as_bytes()[read.len().saturating_sub(6)..];
        let segment = tail.rsplit(|b| *b == b'/').next().unwrap_or(tail);
        let dotted = dots > 0
            && segment.len() < tail.len().max(1)
            && std::str::from_utf8(segment).is_ok_and(|segment| {
                is_dot_segment(&[segment, &rest[..dots]].concat())
            });
        !dotted || passes(&[read, &rest[..dots]].concat())
    };

    let end = url_end(text, at, from)?;
    let authority = authority_end(text, from);
    if (authority..end)
        .any(|i| scheme_at(text, i).is_some() || www_at(bytes, i))
    {
        return None;
    }
    let as_prose = &text[at..end];
    if !passes(as_prose) || !passes_dotted(as_prose, &text[end..]) {
        return None;
    }

    // Each later end: a reading is taken where one could stop, if
    // anything but closing punctuation was written since the last
    let token = run_at(text, at, from)?;
    let bytes = token.as_bytes();
    let mut read = token.starts_with(as_prose).then_some(as_prose.len());
    let mut ends = 0;
    // Where the closing punctuation before `stop` starts
    let mut closed = start.len;
    for stop in start.len..=bytes.len() {
        let here = bytes.get(stop).copied();
        if here.is_none_or(is_bracket) {
            let reading = &token[..closed];
            if !names_no_host(reading, start)
                && !passes_dotted(reading, &token[closed..stop])
            {
                return None;
            }
            if read != Some(closed) {
                ends += 1;
                if ends > MAX_URL_ENDS
                    || !(names_no_host(reading, start) || passes(reading))
                {
                    return None;
                }
                read = Some(closed);
            }
        }
        if here.is_some_and(|b| !is_closing(b)) {
            closed = stop + 1;
        }
    }
    Some(end)
}

/// The text from the URL at `at`, whose authority starts at `from`, to
/// the next whitespace or line break. `None` past [`MAX_URL_BYTES`]
fn run_at(text: &str, at: usize, from: usize) -> Option<&str> {
    let end = bounded_end(text, at, from, is_gap)?;
    Some(&text[at..end])
}

/// The first `stop` in `text` from `from`, or its end; `None` when that is
/// more than [`MAX_URL_BYTES`] past `start`, having read no further
fn bounded_end(
    text: &str,
    start: usize,
    from: usize,
    stop: impl Fn(char) -> bool,
) -> Option<usize> {
    for (offset, c) in text[from..].char_indices() {
        if from + offset - start > MAX_URL_BYTES {
            return None;
        }
        if stop(c) {
            return Some(from + offset);
        }
    }
    (text.len() - start <= MAX_URL_BYTES).then_some(text.len())
}

/// Whether a reading of a URL names no host anyone could reach: the last
/// label of its authority, as written, has punctuation no domain name has
/// (`example.org),next`, `example.org">t<`). Only plain ASCII counts, with
/// nothing a parser decodes or splits on (`%`, `@`, `\`, `[`, `:`, a
/// non-ASCII dot)
fn names_no_host(reading: &str, start: Start) -> bool {
    let rest = &reading[start.len..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let last = authority.rsplit('.').next().unwrap_or(authority);
    authority.is_ascii()
        && !authority.contains(['%', '@', '\\', '[', ':'])
        && last
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
}

/// Punctuation, a bracket or a quote: at a reading's end, not part of it
fn is_closing(b: u8) -> bool {
    is_bracket(b) || is_sentence_punctuation(b)
}

/// Whitespace, or a line break to any reader
fn is_gap(c: char) -> bool {
    c.is_whitespace() || threads::is_line_break(c)
}

/// The end of what a URL parser, or the reader's view before it, drops
/// at `at` (a char boundary): a tab or a line break, and the whitespace
/// before one, which [`threads::tidy`] trims. `at` when nothing is
/// dropped: a space before a word ends a URL
fn skip_dropped(text: &str, at: usize) -> usize {
    let mut end = at;
    for (offset, c) in text[at..].char_indices() {
        if !is_gap(c) {
            break;
        }
        if c == '\t' || threads::is_line_break(c) {
            end = at + offset + c.len_utf8();
        }
    }
    end
}

/// What ends a URL as written in prose, whitespace aside
fn is_bracket(b: u8) -> bool {
    matches!(
        b,
        b'<' | b'>'
            | b'"'
            | b'\''
            | b'`'
            | b'('
            | b')'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'|'
    )
}

/// What is the sentence's and not the URL's, at a URL's end
fn is_sentence_punctuation(b: u8) -> bool {
    matches!(b, b'.' | b',' | b';' | b':' | b'!' | b'?' | b'*' | b'_')
}

/// A scheme at `i`: `http` or `https`, `:`, and any run of `/` and `\`,
/// in any case, with what is dropped ([`skip_dropped`]) anywhere in it.
/// A start wherever it is written. Written any way but `http://` and
/// `https://`, only before a host ([`host_follows`]): `https: ` is prose
fn scheme_at(text: &str, i: usize) -> Option<Start> {
    let bytes = text.as_bytes();
    if matches_at(bytes, i, b"http") {
        let len = if matches_at(bytes, i + 4, b"s://") {
            8
        } else if matches_at(bytes, i + 4, b"://") {
            7
        } else {
            0
        };
        if len > 0 {
            // (a third slash is a parser's to drop: not the one way)
            let plain = !matches!(bytes.get(i + len), Some(b'/' | b'\\'));
            return Some(Start { len, plain });
        }
    } else if !bytes.get(i)?.eq_ignore_ascii_case(&b'h') {
        return None;
    }

    let mut at = i + 1;
    for letter in b"ttp" {
        at = skip_dropped(text, at);
        if !bytes.get(at)?.eq_ignore_ascii_case(letter) {
            return None;
        }
        at += 1;
    }
    at = skip_dropped(text, at);
    if bytes.get(at)?.eq_ignore_ascii_case(&b's') {
        at = skip_dropped(text, at + 1);
    }
    if *bytes.get(at)? != b':' {
        return None;
    }
    // To the last slash: what is dropped after it is not the scheme's
    let mut end = at + 1;
    let mut next = skip_dropped(text, end);
    while let Some(b'/' | b'\\') = bytes.get(next) {
        end = next + 1;
        next = skip_dropped(text, end);
    }
    host_follows(text, next).then_some(Start {
        len: end - i,
        plain: false,
    })
}

/// Whether what is written at `at`, after a scheme, is a host to a URL
/// parser: it has a letter, a digit, `%`, `[` or a non-ASCII byte before
/// the first `/`, `\`, `?`, `#` or space. After a `:` the parser needs a
/// host after an `@` (`http::Request` is Rust; `http::@host` is a URL).
/// Erring towards a host: one it has not found in 256 bytes is there
fn host_follows(text: &str, at: usize) -> bool {
    let bytes = text.as_bytes();
    let mut after_colon = false;
    let mut first = true;
    let mut i = at;
    while let Some(b) = bytes.get(i).copied() {
        if i - at >= 256 {
            return true;
        }
        match b {
            b'/' | b'\\' | b'?' | b'#' => return false,
            // (0x0B and 0x1C to 0x1E are line breaks to the reader's view)
            b' ' | 0x09..=0x0D | 0x1C..=0x1E => {
                let next = skip_dropped(text, i);
                if next == i {
                    return false;
                }
                i = next;
                continue;
            }
            b':' if first => after_colon = true,
            b'@' if after_colon => return true,
            b if !after_colon
                && (b.is_ascii_alphanumeric()
                    || matches!(b, b'%' | b'[')
                    || !b.is_ascii()) =>
            {
                return true;
            }
            _ => {}
        }
        first = false;
        i += 1;
    }
    false
}

fn has_scheme(text: &str) -> bool {
    (0..text.len()).any(|i| scheme_at(text, i).is_some())
}

/// A schemeless `www.` host at `i`, at the start of a word
fn www_at(bytes: &[u8], i: usize) -> bool {
    at_word_boundary(bytes, i)
        && matches_at(bytes, i, b"www.")
        && bytes.get(i + 4).copied().is_some_and(is_authority_byte)
}

/// The end of the authority that starts at `from`: the first byte that
/// cannot be in one, or the start of another URL (`a:https://…`), which
/// is then seen as one
fn authority_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = from;
    while end < bytes.len()
        && is_authority_byte(bytes[end])
        && scheme_at(text, end).is_none()
    {
        end += 1;
    }
    end
}

/// Whether `b` can be in a URL's authority. The first byte that cannot ends
/// what defanging rewrites
fn is_authority_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(b, b'-' | b'.' | b'_' | b'~' | b'%' | b':' | b'@')
}

/// Case-insensitive match of `pat` (ASCII) at `bytes[i..]`
fn matches_at(bytes: &[u8], i: usize, pat: &[u8]) -> bool {
    bytes.len() >= i + pat.len()
        && bytes[i..i + pat.len()]
            .iter()
            .zip(pat)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Whether a schemeless `www.` host can start at `i`: not after a letter,
/// a digit or a dot, so `awww.example.com` and the `www.` of `foo.www.bar`
/// are prose. A scheme needs no boundary ([`scheme_at`])
fn at_word_boundary(bytes: &[u8], i: usize) -> bool {
    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.')
}

/// The end as prose of the URL starting at `start`, whose authority
/// starts at `from`: the first whitespace, line break or bracket, then
/// back over any sentence punctuation before it. `None` past
/// [`MAX_URL_BYTES`]
fn url_end(text: &str, start: usize, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut end = bounded_end(text, start, from, |c| {
        is_gap(c) || c.is_ascii() && is_bracket(c as u8)
    })?;
    while end > from && is_sentence_punctuation(bytes[end - 1]) {
        end -= 1;
    }
    Some(end)
}

/// Whether the URL whose authority starts at `at` is on `site`'s host: a
/// cheap test before any parse, which skips every other site
fn on_site_host(bytes: &[u8], at: usize, site: &Url) -> bool {
    let Some(host) = site.host_str() else {
        return false;
    };
    matches_at(bytes, at, host.as_bytes())
        && bytes
            .get(at + host.len())
            .is_none_or(|b| !is_authority_byte(*b) || *b == b':')
}

/// One rule of a [`Links`] policy, written as a URL: `http://` or
/// `https://`, a host, and optionally a port and a path. No user,
/// password, query or fragment, and nothing a parser would rewrite: a
/// backslash, a control character, a `.` or `..` path segment.
///
/// A URL is matched on its parsed parts ([`Url`]), never as a string
/// prefix, so the host's case and an internationalised name's two spellings
/// do not matter, and neither does one trailing dot after the host.
///
/// - **Host.** Equal, or, for a rule written `*.example.org`, any subdomain
///   of `example.org` (`en.example.org`, `a.b.example.org`) and not
///   `example.org` itself. A leading `*.` before a domain name of two
///   labels or more is the only wildcard: `*.com` and a `*` anywhere else
///   are refused. A public suffix of more labels (`*.co.uk`) cannot be
///   told from a domain without a list of them, and is the operator's to
///   avoid.
/// - **Path.** The rule's segments are the first of the URL's: `/wiki`
///   matches `/wiki` and `/wiki/Rust`, not `/wikipedia`. No path, or `/`,
///   matches every path.
///
/// [`allows`](Self::allows) and [`denies`](Self::denies) differ on the
/// rest, each erring on the side of defanging.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkRule {
    /// The rule without its `*.`, the host without a trailing dot
    url: Url,
    wildcard: bool,
}

/// A string that is not a [`LinkRule`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{given:?} is not a link rule: {reason}")]
pub struct LinkRuleError {
    pub given: String,
    pub reason: LinkRuleRefusal,
}

/// Why a string is not a [`LinkRule`]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LinkRuleRefusal {
    #[error("it is not a URL ({0}); write it like https://example.org/path")]
    NotAUrl(url::ParseError),
    #[error("its scheme is {0:?}; only http and https rules are accepted")]
    Scheme(String),
    #[error(
        "it does not begin `http://` or `https://`; a parser reads it as \
         if it did, and a rule is written the one way"
    )]
    Slashes,
    #[error(
        "it has a backslash, which parsers read differently; a rule is \
         written with `/`"
    )]
    Backslash,
    #[error(
        "it has a control character (a tab or a line break, perhaps), \
         which a parser would drop"
    )]
    Control,
    #[error(
        "it has a user or a password, or the `@` of one; a URL with either \
         matches no allow rule, and every deny rule its host does"
    )]
    Userinfo,
    #[error(
        "it has a query (`?…`); a rule matches on scheme, host, port and path"
    )]
    Query,
    #[error(
        "it has a fragment (`#…`); a rule matches on scheme, host, port and path"
    )]
    Fragment,
    #[error(
        "`*` is a wildcard only as `*.` before a domain name \
         (https://*.example.org), and nowhere else"
    )]
    Wildcard,
    #[error(
        "`*.` is before a single label, so it would match every domain \
         under that name; a wildcard needs two labels or more after it \
         (https://*.example.org)"
    )]
    WildcardSuffix,
    #[error(
        "its host ends in more than one dot; one trailing dot is the most \
         a host has"
    )]
    TrailingDots,
    #[error(
        "its path has a `.` or `..` segment (or `%2e` for one), which a \
         parser would resolve; write the path it resolves to"
    )]
    DotSegment,
}

impl FromStr for LinkRule {
    type Err = LinkRuleError;

    fn from_str(given: &str) -> Result<Self, Self::Err> {
        use LinkRuleRefusal as Why;
        let fail = |reason| LinkRuleError {
            given: given.to_string(),
            reason,
        };
        let written = given.trim();
        if written.contains('\\') {
            return Err(fail(Why::Backslash));
        }
        if written.contains(char::is_control) {
            return Err(fail(Why::Control));
        }
        // `*.` is not part of a host: taken off before the parse
        let (wildcard, parsed) = match written.split_once("://*.") {
            Some((scheme, rest))
                if scheme.bytes().all(|b| b.is_ascii_alphabetic()) =>
            {
                (true, Url::parse(&[scheme, "://", rest].concat()))
            }
            _ => (false, Url::parse(written)),
        };
        let mut url = parsed.map_err(|e| fail(Why::NotAUrl(e)))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(fail(Why::Scheme(url.scheme().to_string())));
        }
        // As written from here: the parser has already normalised `url`
        let Some(rest) = ["https://", "http://"].iter().find_map(|scheme| {
            let (head, rest) = written.split_at_checked(scheme.len())?;
            head.eq_ignore_ascii_case(scheme).then_some(rest)
        }) else {
            return Err(fail(Why::Slashes));
        };
        let (authority, path) = rest
            .find(['/', '?', '#'])
            .map_or((rest, ""), |at| rest.split_at(at));
        if authority.trim_start_matches("*.").is_empty() {
            return Err(fail(Why::Slashes));
        }
        if authority.contains('@')
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(fail(Why::Userinfo));
        }
        if url.query().is_some() {
            return Err(fail(Why::Query));
        }
        if url.fragment().is_some() {
            return Err(fail(Why::Fragment));
        }
        let domain = match url.host() {
            Some(Host::Domain(domain)) => Some(domain.to_string()),
            _ => None,
        };
        if url.as_str().contains('*') || (wildcard && domain.is_none()) {
            return Err(fail(Why::Wildcard));
        }
        if domain.as_deref().is_some_and(|d| d.ends_with("..")) {
            return Err(fail(Why::TrailingDots));
        }
        if path.split('/').any(is_dot_segment) {
            return Err(fail(Why::DotSegment));
        }
        if let Some(absolute) =
            domain.as_deref().and_then(|d| d.strip_suffix('.'))
        {
            url.set_host(Some(absolute))
                .map_err(|e| fail(Why::NotAUrl(e)))?;
        }
        if wildcard && url.host_str().is_some_and(|d| d.split('.').count() < 2)
        {
            return Err(fail(Why::WildcardSuffix));
        }
        Ok(Self { url, wildcard })
    }
}

/// `.` or `..`, with `%2e` for either dot
fn is_dot_segment(segment: &str) -> bool {
    let plain = segment.to_ascii_lowercase().replace("%2e", ".");
    plain == "." || plain == ".."
}

impl fmt::Display for LinkRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rest = &self.url[Position::BeforeHost..];
        let rest = match self.url.path() {
            "/" => rest.strip_suffix('/').unwrap_or(rest),
            _ => rest,
        };
        let wildcard = if self.wildcard { "*." } else { "" };
        write!(f, "{}://{wildcard}{rest}", self.url.scheme())
    }
}

impl LinkRule {
    /// Whether `url` may be left as written under a default of defanging.
    ///
    /// Scheme, host and port (the scheme's own when none is written) are
    /// the rule's, and the path is under the rule's as written: an encoded
    /// spelling of it (`/%77iki`) is not. Never a URL with a user or a
    /// password (`https://example.org@evil.example/`), nor, under a rule
    /// with a path, one whose path has what a server may read as leaving
    /// it: an encoded `/` or `\`, a `;` (`/wiki/..;/admin`), or an encoded
    /// `%` (`%252F`).
    pub fn allows(&self, url: &Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        if url.scheme() != self.url.scheme()
            || url.port_or_known_default() != self.url.port_or_known_default()
            || !self.host_matches(url)
        {
            return false;
        }
        let Some(mut theirs) = url.path_segments() else {
            return false;
        };
        let mut ours = self.segments().peekable();
        if ours.peek().is_none() {
            return true;
        }
        let path = url.path().to_ascii_lowercase();
        if ["%2f", "%5c", "%25", ";"]
            .iter()
            .any(|bad| path.contains(bad))
        {
            return false;
        }
        ours.all(|segment| theirs.next() == Some(segment))
    }

    /// Whether `url` is defanged under a default of leaving URLs as written.
    ///
    /// Looser than [`allows`](Self::allows), so that a rule is not escaped
    /// by another spelling:
    ///
    /// - a user or password is ignored;
    /// - a rule with no port (or its scheme's own, which is the same
    ///   rule) covers every port, `http` and `https` alike, and one with
    ///   a port covers that port under both;
    /// - paths are compared percent-decoded, an encoded `/` or `\` a
    ///   separator like a written one, without their empty segments and
    ///   without a segment's `;parameters`, and both as written and with
    ///   their `..` resolved after the decoding.
    ///
    /// Letter case in a path still counts, so a rule with no path is the
    /// sure one.
    pub fn denies(&self, url: &Url) -> bool {
        let port = self.url.port();
        if port.is_some_and(|port| url.port_or_known_default() != Some(port))
            || !self.host_matches(url)
        {
            return false;
        }
        let ours = loose_segments(self.url.path());
        if ours.is_empty() {
            return true;
        }
        let theirs = loose_segments(url.path());
        let mut resolved = Vec::with_capacity(theirs.len());
        for segment in &theirs {
            match segment.as_str() {
                "." => {}
                ".." => {
                    resolved.pop();
                }
                _ => resolved.push(segment.clone()),
            }
        }
        theirs.starts_with(&ours) || resolved.starts_with(&ours)
    }

    /// The rule's path segments, without the empty last one of `/` and
    /// `/wiki/`
    fn segments(&self) -> impl Iterator<Item = &str> {
        let path = self.url.path();
        path.strip_suffix('/').unwrap_or(path).split('/').skip(1)
    }

    fn host_matches(&self, url: &Url) -> bool {
        match (self.url.host(), url.host()) {
            (Some(Host::Domain(ours)), Some(Host::Domain(theirs))) => {
                let theirs = theirs.strip_suffix('.').unwrap_or(theirs);
                if self.wildcard {
                    // A whole label or more, then a dot, then the rule's
                    theirs
                        .strip_suffix(ours)
                        .and_then(|sub| sub.strip_suffix('.'))
                        .is_some_and(|sub| !sub.is_empty())
                } else {
                    theirs == ours
                }
            }
            (Some(ours), Some(theirs)) => ours == theirs,
            _ => false,
        }
    }
}

/// A path's segments as the loosest server reads them
/// ([`LinkRule::denies`])
fn loose_segments(path: &str) -> Vec<String> {
    percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .split(['/', '\\'])
        .map(|segment| segment.split(';').next().unwrap_or(segment))
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

impl Serialize for LinkRule {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LinkRule {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let given = Cow::<'de, str>::deserialize(d)?;
        given.parse().map_err(serde::de::Error::custom)
    }
}

// A plain string, inline: never a `$ref` (agora CLAUDE.md)
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for LinkRule {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("LinkRule")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed(concat!(module_path!(), "::LinkRule"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "An http or https URL with no user, query or \
                fragment: its scheme, host, port and path prefix are \
                matched. A host written *.example.org matches every \
                subdomain of example.org",
        })
    }
}

#[cfg(test)]
mod tests;
