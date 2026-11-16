//! How Agora's content is shown to a reader: the options a view is built
//! with, the link policy ([`Links`]) applied to text others wrote, and the
//! agent's view of a wire type ([`AgentDisplay`]: text, content blocks and
//! JSON).
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

mod agent;
mod json;

pub use agent::{AgentDisplay, AgentText, DisplayContext};
pub use json::{Exempt, FOLLOWED_URL_KEYS, json_view};

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
/// Every URL start is judged on its own, one inside another URL included,
/// and a URL with another inside it is never left as written: in
/// `https://allowed.example/?u=https://other.example` both are defanged,
/// and in `https://other.example/?u=https://allowed.example` the second is
/// kept. So what is left as written is exactly what was checked.
///
/// What holds of the result, whatever the text:
///
/// - **`defanged` with no `allow` rule** (the default): it has no `http://`
///   and no `https://` in it, in any letter case.
/// - **`raw` with `deny` rules**: no `http://` or `https://` URL in it, read
///   from its scheme to its end as written, is one a rule denies.
///
/// A URL is never left as written on the strength of a rule it cannot be
/// checked against. One with another inside it, one longer than [`MAX_URL_BYTES`], one that does not
/// parse, and one with a backslash or a control character (which parsers
/// read differently) match no `allow` rule, and are defanged under `raw`
/// whenever there is a `deny` rule.
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

/// The longest URL read as a link or checked against a [`LinkRule`]: a
/// longer one matches no rule. It bounds the work per URL
pub const MAX_URL_BYTES: usize = 2_048;

impl Links {
    /// `text` under this policy, with page URLs of `site` as their links.
    /// Borrowed when nothing changes.
    ///
    /// `http://` and `https://`, in any case and wherever they are written
    /// (`xhttp://`, `a:https://`), become `hxxp://` and `hxxps://` and the
    /// dots of the authority `[.]`; a schemeless `www.` host at the start
    /// of a word gets the dots. The path and query are copied.
    ///
    /// Time is linear in `text`: one pass, and each URL start reads at most
    /// [`MAX_URL_BYTES`] ahead and is parsed at most once for the site and
    /// once for the rules. No URL is parsed when there is neither a site
    /// nor a rule.
    pub fn rewrite<'a>(
        &self,
        text: &'a str,
        site: Option<&Url>,
    ) -> Cow<'a, str> {
        let bytes = text.as_bytes();
        let mut out: Option<String> = None;
        let mut i = 0;
        let mut copied = 0; // everything before this offset is already in `out`

        while i < bytes.len() {
            let (scheme_len, https) = if let Some(scheme) = scheme_at(bytes, i)
            {
                scheme
            } else if www_at(bytes, i) {
                (0, false)
            } else {
                i += 1;
                continue;
            };

            // This site's page: its link, which an agent can follow
            if let Some(site) = site.filter(|s| {
                scheme_len > 0 && on_site_host(bytes, i + scheme_len, s)
            }) && let Some(end) = url_end(bytes, i)
                && let Ok(link) = AgoraLink::parse_at(&text[i..end], Some(site))
                    .map(|link| link.to_string())
                // (links percent-encode a URL they carry; were one ever
                // written whole, the page would be any other URL)
                && !has_scheme(link.as_bytes())
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

            if let Some(after) = self.leaves(text, i, scheme_len) {
                i = after;
                continue;
            }

            let buf = out
                .get_or_insert_with(|| String::with_capacity(text.len() + 16));
            buf.push_str(&text[copied..i]);
            if scheme_len > 0 {
                buf.push_str(if https { "hxxps://" } else { "hxxp://" });
            }

            // The dots of the rest of the authority (for a `www.` start,
            // its own included)
            let j = authority_end(bytes, i + scheme_len);
            for b in &bytes[i + scheme_len..j] {
                if *b == b'.' {
                    buf.push_str("[.]");
                } else {
                    buf.push(*b as char);
                }
            }
            copied = j;
            i = j.max(i + 1);
        }

        match out {
            Some(mut buf) => {
                buf.push_str(&text[copied..]);
                Cow::Owned(buf)
            }
            None => Cow::Borrowed(text),
        }
    }

    /// Where to go on from when the URL written at `start` is left as
    /// written: its end, so nothing in it is rewritten after it was checked
    fn leaves(
        &self,
        text: &str,
        start: usize,
        scheme_len: usize,
    ) -> Option<usize> {
        match self {
            Self::Defanged { allow } => {
                if allow.is_empty() || scheme_len == 0 {
                    return None;
                }
                let (url, end) = written_url(text, start, scheme_len)?;
                allow.iter().any(|r| r.allows(&url)).then_some(end)
            }
            // Nothing to check against, and nothing is ever defanged
            Self::Raw { deny } if deny.is_empty() => {
                Some(start + scheme_len.max(1))
            }
            Self::Raw { deny } => {
                let (url, end) = written_url(text, start, scheme_len)?;
                (!deny.iter().any(|r| r.denies(&url))).then_some(end)
            }
        }
    }
}

/// The URL written at `start`, parsed (`http://` put before a schemeless
/// one), and its end. `None` when it is too long, has another URL start
/// inside it, or is one parsers would not agree on
fn written_url(
    text: &str,
    start: usize,
    scheme_len: usize,
) -> Option<(Url, usize)> {
    let bytes = text.as_bytes();
    let end = url_end(bytes, start)?;
    let written = &text[start..end];
    if written.bytes().any(|b| b == b'\\' || b.is_ascii_control()) {
        return None;
    }
    let authority = authority_end(bytes, start + scheme_len);
    if (authority..end)
        .any(|i| scheme_at(bytes, i).is_some() || www_at(bytes, i))
    {
        return None;
    }
    let url = if scheme_len == 0 {
        Url::parse(&["http://", written].concat())
    } else {
        Url::parse(written)
    };
    Some((url.ok()?, end))
}

/// `http://` or `https://` at `i`, in any case: its length, and whether it
/// is `https`. A start wherever it is written
fn scheme_at(bytes: &[u8], i: usize) -> Option<(usize, bool)> {
    if !matches_at(bytes, i, b"http") {
        None
    } else if matches_at(bytes, i + 4, b"s://") {
        Some((8, true))
    } else if matches_at(bytes, i + 4, b"://") {
        Some((7, false))
    } else {
        None
    }
}

fn has_scheme(bytes: &[u8]) -> bool {
    (0..bytes.len()).any(|i| scheme_at(bytes, i).is_some())
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
fn authority_end(bytes: &[u8], from: usize) -> usize {
    let mut end = from;
    while end < bytes.len()
        && is_authority_byte(bytes[end])
        && scheme_at(bytes, end).is_none()
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

/// The end of the URL starting at `start`: the first byte that cannot be in
/// one as written in prose, then back over any sentence punctuation before
/// it. `None` past [`MAX_URL_BYTES`]
fn url_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut end = start;
    while end < bytes.len()
        && !bytes[end].is_ascii_whitespace()
        && !matches!(
            bytes[end],
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
    {
        end += 1;
        if end - start > MAX_URL_BYTES {
            return None;
        }
    }
    while end > start
        && matches!(
            bytes[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b'*' | b'_'
        )
    {
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

/// One rule of a [`Links`] policy, written as a URL: a scheme (`http` or
/// `https`), a host, and optionally a port and a path. No user, password,
/// query or fragment.
///
/// A URL is matched on its parsed parts ([`Url`]), never as a string
/// prefix, so the host's case and an internationalised name's two spellings
/// do not matter, and neither does one trailing dot after the host.
///
/// - **Host.** Equal, or, for a rule written `*.example.org`, any subdomain
///   of `example.org` (`en.example.org`, `a.b.example.org`) and not
///   `example.org` itself. A leading `*.` before a domain name is the only
///   wildcard: a `*` anywhere else in a rule is refused.
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
        "it has a user or a password; a URL with either matches no allow \
         rule, and every deny rule its host does"
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
        if !url.username().is_empty() || url.password().is_some() {
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
        if let Some(absolute) =
            domain.as_deref().and_then(|d| d.strip_suffix('.'))
        {
            url.set_host(Some(absolute))
                .map_err(|e| fail(Why::NotAUrl(e)))?;
        }
        Ok(Self { url, wildcard })
    }
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
    /// with a path, one with an encoded `/` or `\` in its path, which a
    /// server may read as leaving it.
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
        if path.contains("%2f") || path.contains("%5c") {
            return false;
        }
        ours.all(|segment| theirs.next() == Some(segment))
    }

    /// Whether `url` is defanged under a default of leaving URLs as written.
    ///
    /// Looser than [`allows`](Self::allows), so that a rule is not escaped
    /// by another spelling: a user or password is ignored, a rule on its
    /// scheme's own port covers `http` and `https` alike, and paths are
    /// compared percent-decoded, without their empty segments. Letter case
    /// in a path still counts, so a rule with no path is the sure one.
    pub fn denies(&self, url: &Url) -> bool {
        let same_origin = url.scheme() == self.url.scheme()
            && url.port_or_known_default() == self.url.port_or_known_default();
        let both_default = url.port().is_none() && self.url.port().is_none();
        if !(same_origin || both_default) || !self.host_matches(url) {
            return false;
        }
        let Some(theirs) = url.path_segments() else {
            return false;
        };
        let decoded = |segment| {
            percent_encoding::percent_decode_str(segment).decode_utf8_lossy()
        };
        let mut theirs = theirs.filter(|s| !s.is_empty()).map(decoded);
        self.segments()
            .filter(|s| !s.is_empty())
            .map(decoded)
            .all(|segment| theirs.next() == Some(segment))
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
