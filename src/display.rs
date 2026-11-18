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

use crate::links::{AgoraLink, SITE_PREFIX};
use crate::names::AgentName;
use crate::threads;

mod agent;
mod domains;
mod json;
#[cfg(test)]
pub(crate) mod oracle;
mod profiles;
pub(crate) mod shown_once;

#[cfg(any(test, all(feature = "seed", feature = "misanthropic")))]
pub(crate) use agent::views;
pub use agent::views::{
    Dashboard, MODEL_LINE_PREFIX, ModelName, model_line, replace_model_line,
};
pub use agent::{AgentDisplay, AgentText, ByAuthor, DisplayContext};
pub use domains::{
    DomainChange, DomainFilter, DomainNote, NotSent, ServerTool, ToolDomains,
    ToolOffer, Widening,
};
pub use json::{AgentJson, Authored, Exempt, Unless, json_view};
pub use profiles::{
    AgentDisplayProfiles, DisplaySelection, ProfileName, ProfileNameError,
    ResolvedDisplay, SelectedBy, UnknownProfile,
};
pub use shown_once::{
    BLOCKED_NOTE, Cues, DEFANGED_AUTHORS_NOTE, DEFANGED_NOTE, Shown, ShownOnce,
    defanged_note_for,
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
/// link ([`AgoraLink::on_site`]), and so is a path on the site right after
/// `](` or `](<`, as a Markdown inline link's target is written
/// (`[post](/agora/c/news/<id>)`), and nowhere else. That is by the
/// characters, not by Markdown: a code span, fenced code, an image's
/// target and a stray `](/agora/…` are rewritten too, as a full URL in
/// code is, and only a link of the site's own can come of it. A page under
/// `/agora/` that no kind names is `agora://page/<path>?<query>` (its
/// `#fragment` dropped).
///
/// Run the policy on the stored text, once: over text it already rendered
/// it is not idempotent for a relative target that held a defanged URL
/// (nothing followable results, but the text differs). A URL on
/// the site is treated as any other URL when it is outside `/agora/` (the
/// host's other pages are not Agora's), has a `.` or `..` segment
/// (between `/` or `\`), a control character or a user, or another URL in
/// its path or query (`?next=https://…`, `?u=www.evil.example`): a link
/// written whole would carry it, so a link is written only when the
/// default policy would leave all of it. A page the site served before
/// its paths were the links' (`/agora/governance/GOV-…`) is the link it
/// names, with the page's query if that link takes it and without it if
/// not.
///
/// A URL starts at `http` or `https`, a `:` and any run of `/` and `\`
/// (`https://x`, `https:/x`, `https:\\x`), in any letter case, with tabs
/// and line breaks anywhere in it, which URL parsers drop: whatever a
/// WHATWG parser reads as `http` or `https` with a host. With no slash
/// (`https:evil.com`), only before a domain name: a dot, and a last label
/// of two letters or more (`HTTP:200`, `{http:1.1}` and `http:Agent` are
/// code). Here
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
///   dropped, read one with a host that has a letter or a digit, but a
///   slashless one to a host that is not a domain name (see above).
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
/// one that ends as prose does (before any of ``<>"'`()[]{}|``, and, in
/// the authority, before punctuation no host name has or a mark: see
/// below), and each that ends at a later one of those, at a mark in the
/// path, or at the whitespace. Sentence punctuation (`.,;:!?*_`), `~`,
/// brackets, quotes and marks at the end of a reading are not part of
/// it. A mark is Unicode punctuation (`“ ” — … 、 （`), a superscript or
/// a character a parser ignores (a zero-width space, a soft hyphen), from
/// a hand-kept list. Each reading is checked alone: under `allow` each
/// must be allowed, by any rule. A later reading whose host ends in a
/// label with punctuation no domain name has (`example.org),next`,
/// `example.org">t<`, `docs.rs[1]`, `docs.rs—the`) names nothing to fetch
/// and is not checked.
///
/// A URL is never left as written on the strength of a rule it cannot be
/// checked against. With rules, each of these is defanged, allowed or
/// not denied as it may be: a URL with another inside it, before its
/// first bracket or quote (the one inside is judged on its own, as every
/// URL start is); one with a reading that does not parse, or that has a
/// backslash or a control character; one whose scheme is not written
/// `http://` or `https://`; one longer than [`MAX_URL_BYTES`] to the
/// next space; and one with more than 16 readings that could name
/// another host, or, under a rule with a path, more than 16 of its other
/// paths to parse (those that change nothing a rule reads are not).
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
/// - Formatting that runs after the policy can change what follows a URL
///   left as written ([`DisplayContext::on_line`] makes `...` of `⋯`).
/// - A slashless URL to a single-label or numeric host (`http:1.1`,
///   `https:localhost`): far likelier code than a link.
/// - A URL that ends where a rule's path does, with more after a quote
///   and no space (`"https://docs.rs/serde","k":1`), under that rule: read
///   to the space its last segment is not the rule's, so it is defanged.
///
/// **Authors.** Under `defanged`, `allow_authors` lists agents, by name,
/// whose own text is shown with its links as written, whatever `allow`
/// says: text a view knows they wrote ([`DisplayContext::by`],
/// [`Authored`]), and nothing they are merely quoted in. That text is read
/// as under `raw` with no rule: a page URL of the site is still its
/// `agora://` link, and nothing is defanged. `raw` has no author list
/// (nothing is defanged there that one could relax), and refuses the key.
///
/// "Author" is whoever wrote the field, not whoever first wrote a link in
/// it: a link a listed author quotes or reposts from someone else is
/// shown as written too, as the server's trusted tier did. List only
/// registered agents: a name no one has registered yet can be registered
/// by anyone (a typo is the risk), and a config cannot check offline that
/// a name exists. A name listed twice is refused.
///
/// ```toml
/// links = { mode = "defanged", allow = ["https://en.wikipedia.org/wiki"] }
/// links = { mode = "defanged", allow_authors = ["claude-ai", "steward"] }
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
        /// Agents whose own text is shown with its links as written
        /// ([`Links::allows_author`]), by name: a name never changes, and
        /// is never another agent's (deleted agents keep theirs)
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "author_names"
        )]
        allow_authors: Vec<AgentName>,
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
        Self::Defanged {
            allow: Vec::new(),
            allow_authors: Vec::new(),
        }
    }
}

/// What a listed author's text is read under ([`Links::for_author`])
static AS_WRITTEN: Links = Links::Raw { deny: Vec::new() };

/// The default, every off-site URL defanged: what a site page's link is
/// checked against before it is written
static DEFAULT: Links = Links::Defanged {
    allow: Vec::new(),
    allow_authors: Vec::new(),
};

/// `allow_authors`: agent names, each refused with the key named, and a
/// UUID refused as one (it passes the name rule, and would list no one)
fn author_names<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<AgentName>, D::Error> {
    Vec::<String>::deserialize(d)?
        .into_iter()
        .map(|given| {
            if given.parse::<uuid::Uuid>().is_ok() {
                return Err(serde::de::Error::custom(format_args!(
                    "allow_authors: {given:?} is an agent id; authors are \
                     listed by name (the handle, like `claude-ai`)"
                )));
            }
            given.parse::<AgentName>().map_err(|e| {
                serde::de::Error::custom(format_args!(
                    "allow_authors: {given:?}: {e}"
                ))
            })
        })
        .try_fold(Vec::new(), |mut names: Vec<AgentName>, name| {
            let name = name?;
            // Refused, not merged: `check` would count one author twice
            if names.contains(&name) {
                return Err(serde::de::Error::custom(format_args!(
                    "allow_authors: {name:?} is listed twice"
                )));
            }
            names.push(name);
            Ok(names)
        })
}

/// Whether `text` shows a link as the policy defangs one: a scheme
/// written `hxxp`, or a host `www[.]`, in any letter case and with what
/// the scanner drops inside a scheme dropped here too (a tab or a line
/// break, and the whitespace before one: `h\nxxps://`). For a reader's
/// one-time note on what such a link is, asked of the text the reader is
/// given, never of what a view read before it cut or left a field out.
/// Text that writes one itself (`hxxps://` in a post about defanging)
/// counts too: the note explains it all the same
pub fn shows_defanged(text: &str) -> bool {
    let mut read = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\t' || threads::is_line_break(c) {
            // and the whitespace before it, as `skip_dropped` reads it
            while read.ends_with(|c: char| is_gap(c)) {
                read.pop();
            }
            continue;
        }
        read.push(c.to_ascii_lowercase());
    }
    read.contains("hxxp") || read.contains("www[.]")
}

/// The longest URL read as a link or checked against a [`LinkRule`],
/// from its scheme to the next space: a longer one matches no rule. It
/// bounds the work per URL
pub const MAX_URL_BYTES: usize = 2_048;

/// The most readings of one URL that could name another host, and the
/// most of its readings with another path that are parsed, that are
/// checked against the rules (see [`Links`]): one with more is not left
/// as written. With [`MAX_URL_BYTES`] it bounds the work per URL
const MAX_URL_ENDS: usize = 16;

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
    /// Whether `author`'s own text is shown with its links as written:
    /// `allow_authors` has the name, exactly. Names are compared byte for
    /// byte, case included, as [`AgentName`]'s `Eq` is: two agents may
    /// differ by case alone (`Oracle` and `oracle`)
    pub fn allows_author(&self, author: &AgentName) -> bool {
        match self {
            Self::Defanged { allow_authors, .. } => {
                allow_authors.contains(author)
            }
            Self::Raw { .. } => false,
        }
    }

    /// The policy for text `author` wrote: this one, or for a listed
    /// author ([`allows_author`](Self::allows_author)) `raw` with no rule,
    /// so a site page is still its link and nothing else is touched.
    /// `None` (no author known) is this one
    pub fn for_author(&self, author: Option<&AgentName>) -> &Links {
        match author {
            Some(author) if self.allows_author(author) => &AS_WRITTEN,
            _ => self,
        }
    }

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
    /// most [`MAX_URL_BYTES`] ahead and parses at most a few dozen of its
    /// readings. No URL is parsed when there is neither a site nor a rule.
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
            // A path on the site as a Markdown link's target
            // (`](/agora/c/news/<id>)`): its link, as its full URL would be
            if let Some(site) = site
                && bytes[i] == b'/'
                && let Some(end) = relative_target(text, i)
                && let Some(link) = AgoraLink::on_site_path(&text[i..end], site)
                    .map(|link| link.to_string())
                && matches!(DEFAULT.rewrite(&link, None), Cow::Borrowed(_))
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
            let start = if let Some(scheme) = scheme_at(text, i) {
                scheme
            } else if www_at(bytes, i) {
                Start::WWW
            } else {
                i += 1;
                continue;
            };
            let authority = i + start.len;

            // This site's page: its link, which an agent can follow, or
            // for a page no kind names, `agora://page/…` (never whole: a
            // page of the site shows others' links as written)
            if let Some(site) = site
                .filter(|s| start.plain && on_site_host(bytes, authority, s))
                && let Some(end) = url_end(text, i, authority)
                && let Some(link) = AgoraLink::on_site(&text[i..end], site)
                    .map(|link| link.to_string())
                // (links percent-encode a URL they carry; were one ever
                // written whole, the page would be any other URL)
                // Nothing in it the plain policy would touch: a scheme or
                // a `www.` host in its path or query (`?u=www.evil.example`)
                // is a URL a whole link would carry
                && matches!(DEFAULT.rewrite(&link, None), Cow::Borrowed(_))
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
            Self::Defanged { allow, .. } => {
                if allow.is_empty() || start.len == 0 {
                    return None;
                }
                every_reading(text, at, start, allow, |url| {
                    allow.iter().any(|r| r.allows(url))
                })
            }
            // Nothing to check against, and nothing is ever defanged
            Self::Raw { deny } if deny.is_empty() => {
                Some(at + start.len.max(1))
            }
            Self::Raw { deny } => every_reading(text, at, start, deny, |url| {
                !deny.iter().any(|r| r.denies(url))
            }),
        }
    }
}

/// The most path segments of any of `rules` on `url`'s host: 0 when none
/// has a path, and the path of a URL there then decides nothing
fn depth(rules: &[LinkRule], url: &Url) -> usize {
    rules
        .iter()
        .filter(|r| r.host_matches(url))
        .map(|r| r.segments().count())
        .max()
        .unwrap_or(0)
}

/// The end as prose of the URL written at `at`, when every reading of it
/// (see [`Links`]) parses and passes (`http://` put before a schemeless
/// one). `None` too when it has another URL start inside it, is too long
/// or has too many readings.
///
/// `rules` are those `pass` checks: a reading that ends in the path
/// differs from a shorter one in what a rule sees only by its path, so
/// with no rule on its host that has one ([`depth`]) it is not parsed
/// again, and with one it is not where nothing a rule reads has changed
/// ([`Settled`])
fn every_reading(
    text: &str,
    at: usize,
    start: Start,
    rules: &[LinkRule],
    pass: impl Fn(&Url) -> bool,
) -> Option<usize> {
    if start.len > 0 && !start.plain {
        return None;
    }
    let bytes = text.as_bytes();
    let from = at + start.len;
    let parse = |reading: &str| {
        if start.len == 0 {
            Url::parse(&["http://", reading].concat())
        } else {
            Url::parse(reading)
        }
    };
    let passes = |reading: &str| {
        if reading.bytes().any(|b| b == b'\\' || b.is_ascii_control()) {
            return false;
        }
        parse(reading).is_ok_and(|url| pass(&url))
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

    let token = run_at(text, at, from)?;
    // Where its path starts (a `\` is refused, not a separator: some
    // parsers read the authority past it), and what the authority holds
    let path_from = token[start.len..]
        .find(['/', '?', '#'])
        .map_or(token.len(), |o| start.len + o);
    let scan = AuthorityScan::of(token, start.len, path_from);
    // The path segments a rule on the path-only readings' host reads
    // (every one, if that does not parse: nothing is skipped)
    let depth =
        parse(&token[..path_from]).map_or(usize::MAX, |url| depth(rules, &url));
    // A path-only reading names a host only if the authority does
    let path_no_host = scan.no_host(token, start, path_from, parse);
    // The first backslash or control character: a reading with one is
    // refused, as `passes` would
    let bad = token
        .find(|c: char| c == '\\' || c.is_ascii_control())
        .unwrap_or(token.len());

    // Each later end: a reading is taken where one could stop, if
    // anything but closing punctuation was written since the last
    let mut read = token.starts_with(as_prose).then_some(as_prose.len());
    // The last path-only reading parsed and passed
    let mut settled = (as_prose.len() > path_from)
        .then(|| Settled::of(token, path_from, as_prose.len(), depth));
    let (mut ends, mut paths) = (0, 0);
    // Where the closing punctuation before `stop` starts
    let mut closed = start.len;
    let stops = token[start.len..]
        .char_indices()
        .map(|(o, c)| (start.len + o, Some(c)))
        .chain([(token.len(), None)]);
    for (stop, here) in stops {
        let is_stop = here.is_none_or(|c| {
            is_mark(c)
                || (c.is_ascii() && is_bracket(c as u8))
                || (stop < path_from && ends_host(c))
        });
        if is_stop && closed <= path_from {
            // It can name another host
            // (one decided by its punctuation costs nothing, and is not
            // counted; any other is)
            let reading = &token[..closed];
            let punctuated = scan.punctuated(closed);
            if read != Some(closed) && !punctuated {
                ends += 1;
                if ends > MAX_URL_ENDS {
                    return None;
                }
            }
            let no_host =
                punctuated || scan.no_host(token, start, closed, parse);
            if !no_host
                && (!passes_dotted(reading, &token[closed..stop])
                    || (read != Some(closed) && !passes(reading)))
            {
                return None;
            }
            read = Some(closed);
        } else if is_stop && !path_no_host {
            // Another path on the same authority
            let reading = &token[..closed];
            let dots = &token[closed..stop];
            let dots = &dots[..dots.bytes().take_while(|b| *b == b'.').count()];
            if read != Some(closed) || !dots.is_empty() {
                if closed > bad {
                    return None;
                }
                let same = match settled {
                    Some(_) if depth == 0 => true,
                    Some(p) => p
                        .extend(token, closed, dots)
                        .map(|next| {
                            settled = Some(next);
                        })
                        .is_some(),
                    None => false,
                };
                if !same {
                    paths += 1;
                    if paths > MAX_URL_ENDS
                        || !passes_dotted(reading, dots)
                        || (read != Some(closed) && !passes(reading))
                    {
                        return None;
                    }
                    settled =
                        Some(Settled::of(token, path_from, closed, depth));
                }
                read = Some(closed);
            }
        }
        if here.is_some_and(|c| !is_closing(c)) {
            closed = stop + here.map_or(0, char::len_utf8);
        }
    }
    Some(end)
}

/// A path-only reading that was parsed and passed, and what a longer
/// one may add to it and be passed or refused as it was, given rules
/// whose paths have at most `depth` segments: the first `depth` segments
/// are settled (to every reader: written, decoded and without
/// `;parameters`), it has no dot segment ([`has_dot_segment`]), and what
/// is added makes none and decodes nothing. A query or fragment already
/// begun settles the path whole. Checked a piece at a time, so the work
/// per URL stays linear
#[derive(Debug, Clone, Copy)]
struct Settled {
    /// Where the reading ends in its token
    end: usize,
    tail: Tail,
}

/// What the last piece of a settled path is (between `/`, `\` or `;`,
/// decoded)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// A query or fragment has begun
    Fixed,
    /// Empty or only dots: more dots would make a dot segment
    Dots,
    Other,
    /// Nothing is known to add to it: its first segments are not settled,
    /// it has a dot segment, or a `%` near its end could join what follows
    Open,
}

impl Settled {
    fn of(token: &str, path_from: usize, end: usize, depth: usize) -> Self {
        let path = &token[path_from..end];
        let tail = if path.contains(['?', '#']) {
            Tail::Fixed
        } else if has_dot_segment(path)
            || path.bytes().filter(|b| *b == b'/').count() <= depth
            || loose_segments(path).len() <= depth
        {
            Tail::Open
        } else {
            let last = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let near_end = &last.as_bytes()[last.len().saturating_sub(2)..];
            if last.to_ascii_lowercase().contains("%25")
                || near_end.contains(&b'%')
            {
                Tail::Open
            } else {
                let decoded = percent_encoding::percent_decode_str(last)
                    .decode_utf8_lossy();
                piece_tail(
                    decoded.rsplit(['/', '\\', ';']).next().unwrap_or(""),
                )
            }
        };
        Self { end, tail }
    }

    /// The reading `token[..to]`, settled as this one is, when it (and
    /// `dots` after it) is passed or refused as this one was
    fn extend(&self, token: &str, to: usize, dots: &str) -> Option<Self> {
        // (the reading as prose may be longer than one taken before it)
        let added = token.get(self.end..to)?;
        let tail = match self.tail {
            Tail::Fixed => return Some(Self { end: to, ..*self }),
            Tail::Open => return None,
            tail => tail,
        };
        let mut checked = String::with_capacity(added.len() + dots.len());
        checked.push_str(added);
        checked.push_str(dots);
        if checked.contains('%') {
            return None;
        }
        let mut pieces = checked.split(['/', '\\', ';']);
        let first = pieces.next().unwrap_or("");
        let dotted =
            |piece: &str| !piece.is_empty() && piece.bytes().all(|b| b == b'.');
        if (tail == Tail::Dots
            && first.bytes().all(|b| b == b'.')
            && !first.is_empty())
            || pieces.any(dotted)
        {
            return None;
        }
        let tail = if added.contains(['?', '#']) {
            Tail::Fixed
        } else if let Some((_, last)) = added.rsplit_once(['/', '\\', ';']) {
            piece_tail(last)
        } else if tail == Tail::Dots && added.bytes().all(|b| b == b'.') {
            Tail::Dots
        } else {
            Tail::Other
        };
        Some(Self { end: to, tail })
    }
}

fn piece_tail(piece: &str) -> Tail {
    if piece.bytes().all(|b| b == b'.') {
        Tail::Dots
    } else {
        Tail::Other
    }
}

/// What a URL's authority, `token[at..path_from]`, holds before each
/// place: where the first character is that a parser decodes or splits on
/// (`%`, `@`, `\`, a control character, or `[` first), the first `:`, and,
/// for each place in the host before that `:`, whether the label it ends
/// has ASCII punctuation no host name has
struct AuthorityScan {
    at: usize,
    path_from: usize,
    special: usize,
    colon: usize,
    /// By offset from `at`: whether the label ending there is punctuated
    punctuated: Vec<bool>,
}

impl AuthorityScan {
    fn of(token: &str, at: usize, path_from: usize) -> Self {
        let authority = &token.as_bytes()[at..path_from];
        let find = |f: &dyn Fn(u8) -> bool| {
            authority
                .iter()
                .position(|b| f(*b))
                .map_or(usize::MAX, |o| at + o)
        };
        let colon = find(&|b| b == b':');
        let mut special =
            find(&|b| matches!(b, b'%' | b'@' | b'\\') || b.is_ascii_control());
        if authority.first() == Some(&b'[') {
            special = at;
        }
        let host = &token[at..colon.min(path_from)];
        let mut punctuated = Vec::with_capacity(host.len() + 1);
        let mut label = false;
        punctuated.push(false);
        for c in host.chars() {
            if is_dot(c) {
                label = false;
            } else if c.is_ascii_punctuation() && !matches!(c, '-' | '_') {
                label = true;
            }
            punctuated.extend(std::iter::repeat_n(label, c.len_utf8()));
        }
        Self {
            at,
            path_from,
            special,
            colon,
            punctuated,
        }
    }

    /// Whether the reading `token[..end]`, ending in the authority, names
    /// no host for its ASCII punctuation alone: decided at once
    fn punctuated(&self, end: usize) -> bool {
        let end = end.min(self.path_from);
        self.special >= end && self.punctuated[end.min(self.colon) - self.at]
    }

    /// Whether the reading `token[..end]` names no host anyone could
    /// reach, as far as its authority says (see [`Links`]): its host has
    /// ASCII punctuation no host name has (`example.org),next`,
    /// `example.org">t<`, `docs.rs[1]`), or its last label has a mark
    /// ([`is_mark`]) a parser keeps in it (`docs.rs—the`). Never one with
    /// a user, a `%`, a `\` or an IPv6 address, which a parser reads
    /// otherwise
    fn no_host(
        &self,
        token: &str,
        start: Start,
        end: usize,
        parse: impl Fn(&str) -> Result<Url, url::ParseError>,
    ) -> bool {
        let end = end.min(self.path_from);
        if self.special < end {
            return false;
        }
        if self.punctuated(end) {
            return true;
        }
        let host_end = end.min(self.colon);
        let host = &token[start.len..host_end];
        let last = host.rsplit(is_dot).next().unwrap_or(host);
        if !last.chars().any(|c| is_mark(c) && !maps_away(c)) {
            return false;
        }
        // Unless a parser maps the mark to what a host has (`․` is a dot)
        match parse(&token[..end]) {
            Err(_) => true,
            Ok(url) => url.host_str().is_some_and(|host| {
                let host = host.strip_suffix('.').unwrap_or(host);
                let last = host.rsplit('.').next().unwrap_or(host);
                last.starts_with("xn--")
                    || last.bytes().any(|b| {
                        !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    })
            }),
        }
    }
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
    mut stop: impl FnMut(char) -> bool,
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

/// Punctuation, a bracket, a quote or a mark: at a reading's end, not
/// part of it
fn is_closing(c: char) -> bool {
    if c.is_ascii() {
        let b = c as u8;
        is_bracket(b) || is_sentence_punctuation(b) || b == b'~'
    } else {
        is_mark(c)
    }
}

/// What ends a host where it is written: ASCII punctuation no host name
/// has (all but `-`, `.` and `_`, and the `:`, `@`, `[`, `]` and `%` of
/// an authority's structure), and a mark ([`is_mark`])
fn ends_host(c: char) -> bool {
    is_mark(c)
        || (c.is_ascii_punctuation()
            && !matches!(c, '-' | '.' | '_' | ':' | '@' | '[' | ']' | '%'))
}

/// A dot to a host name's parser: `.`, and the three it maps to one
fn is_dot(c: char) -> bool {
    matches!(c, '.' | '\u{3002}' | '\u{FF0E}' | '\u{FF61}')
}

/// Whether `c` ends a URL in prose and is not part of one at its end: a
/// hand-kept list, not a Unicode property. Not ASCII, and not a letter
/// or a digit (an internationalised host and `/wiki/Café` go on):
///
/// - punctuation: Latin-1's (`¡ § « ¶ · » ¿`), General Punctuation
///   (U+2000 to U+206F: dashes, quotes, `…`, `‰`, `※`), Supplemental
///   Punctuation (U+2E00 to U+2E7F), CJK Symbols and Punctuation (U+3000
///   to U+303F), the vertical, compatibility and small forms (U+FE10 to
///   U+FE6F), the full- and half-width forms' (`，`, `（`, `｡`), and the
///   commonest of other scripts (Greek, Armenian, Hebrew, Arabic,
///   Devanagari, Thai, Ethiopic);
/// - superscripts and fractions: `¹ ² ³ ¼ ½ ¾` and U+2070 to U+209F
///   (footnote marks);
/// - what a parser ignores ([`maps_away`]): soft hyphen, zero-width
///   characters, bidi controls, variation selectors, BOM, tags
fn is_mark(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    if maps_away(c) {
        return true;
    }
    let punctuation = matches!(
        c,
        '\u{A1}' | '\u{A7}' | '\u{AB}' | '\u{B6}' | '\u{B7}' | '\u{BB}'
            | '\u{BF}'
            | '\u{2000}'..='\u{206F}'
            | '\u{2E00}'..='\u{2E7F}'
            | '\u{3000}'..='\u{303F}'
            | '\u{FE10}'..='\u{FE1F}'
            | '\u{FE30}'..='\u{FE6F}'
            | '\u{FF01}'..='\u{FF0F}'
            | '\u{FF1A}'..='\u{FF20}'
            | '\u{FF3B}'..='\u{FF40}'
            | '\u{FF5B}'..='\u{FF65}'
            // Greek, Armenian, Hebrew
            | '\u{37E}' | '\u{387}' | '\u{55A}'..='\u{55F}' | '\u{589}'
            | '\u{58A}' | '\u{5BE}' | '\u{5C0}' | '\u{5C3}' | '\u{5C6}'
            | '\u{5F3}' | '\u{5F4}'
            // Arabic, Devanagari, Thai, Ethiopic
            | '\u{60C}' | '\u{60D}' | '\u{61B}' | '\u{61E}' | '\u{61F}'
            | '\u{66A}'..='\u{66D}' | '\u{6D4}' | '\u{964}' | '\u{965}'
            | '\u{970}' | '\u{E5A}' | '\u{E5B}' | '\u{1360}'..='\u{1368}'
    );
    let superscript = matches!(
        c,
        '\u{B2}' | '\u{B3}' | '\u{B9}' | '\u{BC}'..='\u{BE}' | '\u{2070}'..='\u{209F}'
    );
    superscript || (punctuation && !c.is_alphanumeric())
}

/// What a host name's parser drops or maps to nothing, and a reader does
/// not see: Unicode's default-ignorable code points, as a list
fn maps_away(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{34F}'
            | '\u{61C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
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
/// `https://`, only before a host ([`host_follows`]): `https: ` is prose;
/// and with no slash, only before a domain name ([`domain_follows`]):
/// `HTTP:200` and `http:Agent` are code
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
    let follows = if end > at + 1 {
        host_follows(text, next)
    } else {
        domain_follows(text, next)
    };
    follows.then_some(Start {
        len: end - i,
        plain: false,
    })
}

/// Whether what is written at `at`, after a scheme, is a host to a URL
/// parser: it has a letter, a digit, `%`, `[` or a non-ASCII character
/// that is not a space before the first `/`, `\`, `?`, `#` or space.
/// After a `:` the parser needs a host after an `@` (`http::Request` is
/// Rust; `http::@host` is a URL). Erring towards a host: one it has not
/// found in 256 bytes is there
fn host_follows(text: &str, at: usize) -> bool {
    let bytes = text.as_bytes();
    let mut after_colon = false;
    let mut first = true;
    let mut i = at;
    while let Some(c) = text[i..].chars().next() {
        if i - at >= 256 {
            return true;
        }
        if is_gap(c) {
            let next = skip_dropped(text, i);
            if next == i {
                return false;
            }
            i = next;
            continue;
        }
        match bytes[i] {
            b'/' | b'\\' | b'?' | b'#' => return false,
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
        i += c.len_utf8();
    }
    false
}

/// Whether what is written at `at`, after a scheme with no slash
/// (`https:evil.com`), is a domain name: up to the first `/`, `\`, `?`,
/// `#` or space, and at any bracket, quote or mark before that, its host
/// (after an `@`, before a port) has a dot and ends in a label of two
/// letters or more, or an `xn--` one. So `HTTP:200`, `http:Agent`,
/// `{http:1.1}`, `http:proxy:3128` and `https:localhost` are not URLs
/// here, though a parser reads each as one: a slashless URL to a
/// single-label or numeric host is not seen
fn domain_follows(text: &str, at: usize) -> bool {
    let mut i = at;
    let mut written = String::new();
    while let Some(c) = text[i..].chars().next() {
        if i - at >= 256 {
            return true;
        }
        if is_gap(c) {
            let next = skip_dropped(text, i);
            if next == i {
                break;
            }
            i = next;
            continue;
        }
        if matches!(c, '/' | '\\' | '?' | '#') {
            break;
        }
        if (c.is_ascii() && is_bracket(c as u8) || is_mark(c))
            && is_domain(&written)
        {
            return true;
        }
        written.push(c);
        i += c.len_utf8();
    }
    is_domain(&written)
}

/// Whether the host in `written` (after its last `@`, before a port,
/// percent-decoded, what a parser ignores dropped, and without closing
/// punctuation at its end) is a domain name: see [`domain_follows`]. Not
/// one with a character a parser refuses in a host
fn is_domain(written: &str) -> bool {
    let host = written.rsplit('@').next().unwrap_or(written);
    let host = host.split(':').next().unwrap_or(host);
    let host: String = percent_encoding::percent_decode_str(host)
        .decode_utf8_lossy()
        .chars()
        .filter(|c| !maps_away(*c))
        .collect();
    let host = host.trim_end_matches(is_closing);
    // What no parser takes for a host (`["https:"].iter().any`)
    if host.contains(['<', '>', '[', ']', '^', '|', '\\']) {
        return false;
    }
    let Some((_, last)) = host.rsplit_once(is_dot) else {
        return false;
    };
    let ace = last.len() > 4 && last[..4].eq_ignore_ascii_case("xn--");
    ace || (last.chars().count() >= 2 && last.chars().all(char::is_alphabetic))
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
/// starts at `from`: the first whitespace, line break or bracket, or, in
/// the authority, the first character that ends a host ([`ends_host`]);
/// then back over any closing punctuation before it ([`is_closing`]).
/// `None` past [`MAX_URL_BYTES`]
fn url_end(text: &str, start: usize, from: usize) -> Option<usize> {
    let mut in_authority = true;
    let mut end = bounded_end(text, start, from, |c| {
        if matches!(c, '/' | '?' | '#' | '\\') {
            in_authority = false;
        }
        is_gap(c)
            || (c.is_ascii() && is_bracket(c as u8))
            || (in_authority && ends_host(c))
    })?;
    while let Some(c) = text[from..end].chars().next_back()
        && is_closing(c)
    {
        end -= c.len_utf8();
    }
    Some(end)
}

/// The end of a path on the site written at `at` as a Markdown inline
/// link's target: after `](` or `](<`, starting [`SITE_PREFIX`], to the
/// first whitespace or `)`, `<`, `>`, `"`, `'`. Nowhere else: a bare
/// `/agora/…` is more often a path someone is talking about. `None` past
/// [`MAX_URL_BYTES`]
fn relative_target(text: &str, at: usize) -> Option<usize> {
    let before = &text[..at];
    if !(before.ends_with("](") || before.ends_with("](<"))
        || !text[at..].starts_with(SITE_PREFIX)
    {
        return None;
    }
    bounded_end(text, at, at, |c| {
        is_gap(c) || matches!(c, ')' | '<' | '>' | '"' | '\'')
    })
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

/// Whether a server may read a `.` or `..` segment in `path`: one written
/// or percent-encoded ([`is_dot_segment`]), encoded twice (`%252e`),
/// between encoded separators (`..%2f`, `..%5c`) or before `;parameters`
/// (`..;/`)
fn has_dot_segment(path: &str) -> bool {
    let mut path = Cow::Borrowed(path);
    for _ in 0..3 {
        if path.split(['/', '\\']).any(|segment| {
            is_dot_segment(segment.split(';').next().unwrap_or(segment))
        }) {
            return true;
        }
        let decoded = match percent_encoding::percent_decode_str(&path)
            .decode_utf8_lossy()
        {
            Cow::Borrowed(_) => return false,
            Cow::Owned(decoded) => decoded,
        };
        path = Cow::Owned(decoded);
    }
    false
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
    /// password (`https://example.org@evil.example/`), nor one whose host
    /// has ASCII punctuation no host name has (`evil.com,.example.org`),
    /// nor, under a rule with a path, one whose path a server may read as
    /// leaving it: with a dot segment in any spelling
    /// ([`has_dot_segment`]: `/wiki/..;/admin`, `..%2f`, `%252e%252e`). An
    /// encoded `/`, a `%25` or a `;` alone is a title (`/wiki/AC%2FDC`).
    pub fn allows(&self, url: &Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        if let Some(Host::Domain(host)) = url.host()
            && !host.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')
            })
        {
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
        if !self.allows_some_paths() {
            return true;
        }
        let mut ours = self.segments();
        if has_dot_segment(url.path()) {
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
        if !self.denies_some_paths() {
            return true;
        }
        let ours = loose_segments(self.url.path());
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

    /// Whether [`allows`](Self::allows) takes only some of a host's paths
    fn allows_some_paths(&self) -> bool {
        self.segments().next().is_some()
    }

    /// Whether [`denies`](Self::denies) takes only some of a host's paths
    fn denies_some_paths(&self) -> bool {
        !loose_segments(self.url.path()).is_empty()
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
