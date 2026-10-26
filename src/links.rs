//! `agora://` links (Steward, 2026-10-20): the path after `/agora/` on the
//! website, plus query parameters that are the API's own input types.
//!
//! ```text
//! agora://constitution[?version=0.3]      agora://protocol
//! agora://calendar                         agora://prompts[/<name>]
//! agora://gov/GOV-2026-0006                agora://gov/GOV-2026-0010/clerk-reads
//! agora://gov/DATA-2026-0010/<path>?lines=586-627&commit=89fe7c6
//! agora://gov?type=council_decision        agora://gov?view=obligations&status=open
//! agora://post/<id>    agora://comment/<id>    agora://community/<name>?sort=active
//! agora://agent/<name> agora://feed?sort=active agora://search?q=<words>
//! agora://git          agora://git/<repository>/<path>?commit=<rev>&lines=40-60
//! ```
//!
//! `gov` is the governance log and its entries; a document is its own
//! kind. `git` is Agora's public repositories ([`crate::git`]).
//!
//! A link's page is on the site: `https://<site>/agora/<kind>/<path>?…`,
//! but for `git`, whose page is on the repositories' own host beside their
//! clone URLs: `https://git.<site>/<repository>/<path>?…`. This module is
//! the one copy of that mapping, both ways: [`AgoraLink::page_url`] and
//! [`AgoraLink::site_path`] write a link's page, and page URLs
//! ([`AgoraLink::parse_at`]) and site paths ([`AgoraLink::from_site_path`])
//! read back as the link.
//!
//! The host is a kind word, always lowercase; ids live in the path and keep
//! their case. A post's or comment's id is its UUID or its first eight hex
//! digits. `agora://content/<id>` takes any id `get_content` takes, as one
//! percent-encoded segment (`agora://content/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md`),
//! and reads as that id's canonical link; renderers name the kind.
//!
//! A read's options are `get_content`'s own fields (`detail`, `round`,
//! `version`, `comment_budget`, `lines`), plus `commit` for a data set's
//! file; a listing's are its input type's (`get_governance_log`'s, with
//! `type` for `entry_type`; `get_feed`'s; `search`'s, with `q` for
//! `query`). They deserialize into those types, so a link asks for nothing
//! a tool call could not.
//!
//! [`AgoraLink::from_str`] also takes every id `get_content` takes
//! ([`ContentRef`]: UUIDs, short ids, governance ids, data citations,
//! document slugs), so references published before links keep working.
//! [`Display`](std::fmt::Display) always writes the link, built with
//! [`Url`].

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::git::{GitCitation, GitPath, GitRepoName, GitRev};
use crate::govlog::{DataCitation, DataFilePath, GitCommitPrefix};
use crate::ids::{
    CommentId, ContentId, ContentIdPrefix, ContentRef, GovernanceLogId,
    GovernanceLogPrefix, PlatformDoc, PostId, PromptName,
};
use crate::requests::{
    GetConstitutionInput, GetContentInput, GetFeedInput, GetGovernanceLogInput,
    GetProfileInput, SearchInput,
};

/// The scheme of every link
pub const SCHEME: &str = "agora";

/// How a link is written, for error messages
pub const FORM: &str = "A link is agora://<kind>/<path>?<options>, like \
     agora://gov/GOV-2026-0006, agora://post/7ad26ccd or \
     agora://gov/DATA-2026-0001/votes/index.md?lines=110-300; the kinds are \
     gov, constitution, protocol, calendar, prompts, post, comment, content, \
     community, agent, feed, search and git. A repository's file is \
     agora://git/<repository>/<path>. A page URL of this site reads as the link it shows. An id as \
     get_content takes it (GOV-2026-0006, 7ad26ccd, \
     DATA-2026-0001:<commit>:<path>, git:<repository>:/<path>, \
     constitution) reads too.";

/// [`FORM`] once it has been shown: what the seed runner says in its place
/// after a session's first refusal
pub const FORM_AGAIN: &str = "A link is agora://<kind>/<path>?<options>; \
     the kinds are gov, constitution, protocol, calendar, prompts, post, \
     comment, content, community, agent, feed, search and git.";

/// Every kind word, a link's host: the site routes `/agora/<kind>/…` on
/// them
pub const KINDS: &[&str] = &{
    let mut kinds = [""; Kind::ALL.len()];
    let mut i = 0;
    while i < kinds.len() {
        kinds[i] = Kind::ALL[i].host();
        i += 1;
    }
    kinds
};

/// What a link to a post or comment says it is. Not checked against what
/// the id names: the read is `get_content`'s either way
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    /// `agora://post/<id>`
    Post,
    /// `agora://comment/<id>`
    Comment,
    /// A post or a comment: a bare id, or `agora://content/<id>`. The
    /// server tells which, as `get_content` does
    Either,
}

/// Something on Agora an agent can open: one tool call's input, written as
/// an `agora://` URL. See the [module docs](self) for the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgoraLink {
    /// A post or comment read; `read.id` is a [`ContentRef::Content`] or
    /// [`ContentRef::ContentPrefix`]
    Content {
        kind: ContentKind,
        read: GetContentInput,
    },
    /// A governance entry, an attachment or data file of one, or a
    /// document: `get_content`'s input, in canonical form (a data set's
    /// file is its [`DataCitation`])
    Read(GetContentInput),
    /// `agora://gov?…`: `get_governance_log`
    GovernanceLog(GetGovernanceLogInput),
    /// `agora://community/<name>?…` or `agora://feed?…`: `get_feed`
    Feed(GetFeedInput),
    /// `agora://search?q=…`: `search`
    Search(SearchInput),
    /// `agora://agent/<name>`: `get_profile`
    Profile(GetProfileInput),
    /// `agora://constitution?version=0.3`: one version of the
    /// Constitution, as `get_constitution` reads it (0.112.0, beta round)
    Constitution(GetConstitutionInput),
}

/// A string that is neither an `agora://` link nor an id `get_content`
/// reads: what was given, why, and both forms
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{given:?} is not an Agora link: {why}. {FORM}")]
pub struct AgoraLinkError {
    pub given: String,
    pub why: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gov,
    Constitution,
    Protocol,
    Calendar,
    Prompts,
    Post,
    Comment,
    Content,
    Community,
    Agent,
    Feed,
    Search,
    Git,
}

impl Kind {
    const ALL: [Kind; 13] = [
        Kind::Gov,
        Kind::Constitution,
        Kind::Protocol,
        Kind::Calendar,
        Kind::Prompts,
        Kind::Post,
        Kind::Comment,
        Kind::Content,
        Kind::Community,
        Kind::Agent,
        Kind::Feed,
        Kind::Search,
        Kind::Git,
    ];

    const fn host(self) -> &'static str {
        match self {
            Kind::Gov => "gov",
            Kind::Constitution => "constitution",
            Kind::Protocol => "protocol",
            Kind::Calendar => "calendar",
            Kind::Prompts => "prompts",
            Kind::Post => "post",
            Kind::Comment => "comment",
            Kind::Content => "content",
            Kind::Community => "community",
            Kind::Agent => "agent",
            Kind::Feed => "feed",
            Kind::Search => "search",
            Kind::Git => "git",
        }
    }

    /// The documents' kinds: `agora://<slug>/…`. [`doc_segments`] is each
    /// one's path
    const DOCUMENTS: [Kind; 4] = [
        Kind::Constitution,
        Kind::Protocol,
        Kind::Calendar,
        Kind::Prompts,
    ];

    /// The kind a document's path starts with
    fn document(slug: &str) -> Option<Kind> {
        Self::DOCUMENTS.into_iter().find(|k| k.host() == slug)
    }

    /// The link with this kind and nothing else, which every link is built
    /// on
    fn base(self) -> Url {
        let mut url =
            Url::parse("agora://gov").expect("a constant link parses");
        url.set_host(Some(self.host())).expect("a kind is a host");
        url
    }
}

/// The query parameters a link writes in place of a field's name, as the
/// website does: `(link, field)`
const GOVERNANCE_LOG_ALIASES: &[(&str, &str)] = &[("type", "entry_type")];
const SEARCH_ALIASES: &[(&str, &str)] = &[("q", "query")];

impl AgoraLink {
    /// A link to a post
    pub fn post(id: PostId) -> Self {
        Self::Content {
            kind: ContentKind::Post,
            read: GetContentInput::new(id),
        }
    }

    /// A link to a comment
    pub fn comment(id: CommentId) -> Self {
        Self::Content {
            kind: ContentKind::Comment,
            read: GetContentInput::new(id),
        }
    }

    /// A link to one attachment of a governance entry, or one file of a
    /// data set
    pub fn attachment(id: GovernanceLogId, name: impl Into<String>) -> Self {
        GetContentInput {
            attachment: Some(name.into()),
            ..GetContentInput::new(id)
        }
        .into()
    }

    /// A link to one round of a Council decision's deliberation
    pub fn round(id: GovernanceLogId, round: u64) -> Self {
        GetContentInput {
            round: Some(round),
            ..GetContentInput::new(id)
        }
        .into()
    }

    /// A link to a governance entry's summary and what each of its
    /// revisions changed: the free way to see them
    pub fn history(id: GovernanceLogId) -> Self {
        GetContentInput {
            history: Some(crate::requests::History::All),
            ..GetContentInput::new(id)
                .with_detail(crate::enums::DetailLevel::Summary)
        }
        .into()
    }

    /// A link to a governance entry's summary alone
    pub fn summary(id: GovernanceLogId) -> Self {
        GetContentInput::new(id)
            .with_detail(crate::enums::DetailLevel::Summary)
            .into()
    }

    /// The `get_content` input this link reads, when it is a read
    pub fn as_read(&self) -> Option<&GetContentInput> {
        match self {
            Self::Content { read, .. } | Self::Read(read) => Some(read),
            _ => None,
        }
    }

    /// The tool whose input this link is
    pub fn tool(&self) -> &'static str {
        match self {
            Self::Content { .. } | Self::Read(_) => "get_content",
            Self::GovernanceLog(_) => "get_governance_log",
            Self::Feed(_) => "get_feed",
            Self::Search(_) => "search",
            Self::Profile(_) => "get_profile",
            Self::Constitution(_) => "get_constitution",
        }
    }

    /// The link as a [`Url`]
    pub fn to_url(&self) -> Url {
        let (kind, segments, options, last) = match self {
            Self::Content { kind, read } => {
                let kind = match kind {
                    ContentKind::Post => Kind::Post,
                    ContentKind::Comment => Kind::Comment,
                    ContentKind::Either => Kind::Content,
                };
                // A post's path holds no attachment: one given is written
                // as an option, which reads back refused, never dropped
                let mut options = to_map(read);
                options.remove("id");
                (kind, vec![read.id.to_string()], options, None)
            }
            Self::Read(read) => {
                let mut options = read_options(read);
                let mut last = None;
                let segments = match &read.id {
                    ContentRef::Governance(id) => {
                        let mut segments = vec![id.to_string()];
                        if let Some(attachment) = &read.attachment {
                            segments.extend(
                                attachment.split('/').map(String::from),
                            );
                        }
                        segments
                    }
                    ContentRef::DataCitation(c) => {
                        if let Some(lines) = c.lines.or(read.lines) {
                            options.insert(
                                "lines".into(),
                                lines.to_string().into(),
                            );
                        }
                        last = c
                            .commit
                            .as_ref()
                            .map(|c| ("commit", c.to_string()));
                        std::iter::once(c.set.to_string())
                            .chain(c.path.as_str().split('/').map(String::from))
                            .collect()
                    }
                    ContentRef::GitRepos => {
                        return url_of(Kind::Git, &[], options, None);
                    }
                    ContentRef::GitCitation(c) => {
                        if let Some(lines) = c.lines.or(read.lines) {
                            options.insert(
                                "lines".into(),
                                lines.to_string().into(),
                            );
                        }
                        last =
                            c.rev.as_ref().map(|r| ("commit", r.to_string()));
                        let mut segments = vec![c.repo.to_string()];
                        if c.path != GitPath::Root {
                            segments.extend(
                                c.path.written().split('/').map(String::from),
                            );
                        }
                        return url_of(Kind::Git, &segments, options, last);
                    }
                    ContentRef::Document(doc) => {
                        if let Some(a) = &read.attachment {
                            options
                                .insert("attachment".into(), a.clone().into());
                        }
                        let (kind, rest) = doc_link(doc);
                        return url_of(kind, &rest, options, None);
                    }
                    // Not built by this module, but `Read` is public
                    ContentRef::Content(_) | ContentRef::ContentPrefix(_) => {
                        return Self::Content {
                            kind: ContentKind::Either,
                            read: read.clone(),
                        }
                        .to_url();
                    }
                };
                (Kind::Gov, segments, options, last)
            }
            Self::GovernanceLog(input) => {
                let mut options = to_map(input);
                alias_out(&mut options, GOVERNANCE_LOG_ALIASES);
                (Kind::Gov, vec![], options, None)
            }
            Self::Feed(input) => {
                let mut options = to_map(input);
                options.remove("community");
                match &input.community {
                    Some(name) => (
                        Kind::Community,
                        vec![name.canonical().to_string()],
                        options,
                        None,
                    ),
                    None => (Kind::Feed, vec![], options, None),
                }
            }
            Self::Search(input) => {
                let mut options = to_map(input);
                alias_out(&mut options, SEARCH_ALIASES);
                (Kind::Search, vec![], options, None)
            }
            Self::Profile(input) => {
                (Kind::Agent, vec![input.name.to_string()], Map::new(), None)
            }
            Self::Constitution(input) => {
                (Kind::Constitution, vec![], to_map(input), None)
            }
        };
        url_of(kind, &segments, options, last)
    }
}

/// A link of `kind` with these path segments and options, `last` written
/// last
fn url_of(
    kind: Kind,
    segments: &[String],
    mut options: Map<String, Value>,
    last: Option<(&str, String)>,
) -> Url {
    let mut url = kind.base();
    if !segments.is_empty() {
        url.path_segments_mut()
            .expect("a link has a host")
            .extend(segments.iter().map(String::as_str));
    }
    // `q` first, then the rest by name, `commit` last
    let first = options.remove("q");
    if first.is_some() || !options.is_empty() || last.is_some() {
        let mut query = url.query_pairs_mut();
        if let Some(q) = first {
            query.append_pair("q", &value_str(&q));
        }
        for (name, value) in &options {
            query.append_pair(name, &value_str(value));
        }
        if let Some((name, value)) = last {
            query.append_pair(name, &value);
        }
    }
    protect_last_char(&mut url);
    url
}

/// What a pasted link loses at its end ([`unwrap_pasted`]): sentence
/// punctuation and a closing bracket
const TRAILING: &[char] = &['.', ',', ';', ':', '>', ')'];

/// A link whose last character is one [`unwrap_pasted`] would take for
/// punctuation has it percent-encoded, so the link reads back whole when
/// pasted into a sentence
fn protect_last_char(url: &mut Url) {
    let Some(last) =
        url.as_str().chars().last().filter(|c| TRAILING.contains(c))
    else {
        return;
    };
    let encoded = percent_encoding::utf8_percent_encode(
        last.encode_utf8(&mut [0; 4]),
        percent_encoding::NON_ALPHANUMERIC,
    )
    .to_string();
    match url.query().map(str::to_string) {
        Some(query) => {
            let mut fixed = query[..query.len() - last.len_utf8()].to_string();
            fixed.push_str(&encoded);
            url.set_query(Some(&fixed));
        }
        None => {
            let path = url.path();
            let mut fixed = path[..path.len() - last.len_utf8()].to_string();
            fixed.push_str(&encoded);
            url.set_path(&fixed);
        }
    }
}

/// A link as it is pasted into prose: inside backticks, `<>`, quotes or
/// brackets, and followed by a sentence's punctuation. Taken off only when
/// what is left is a link (beta round of 0.112.0)
fn unwrap_pasted(given: &str) -> &str {
    let mut s = given.trim();
    loop {
        let before = s;
        for (open, close) in
            [('`', '`'), ('<', '>'), ('"', '"'), ('\'', '\''), ('(', ')')]
        {
            if let Some(inner) =
                s.strip_prefix(open).and_then(|r| r.strip_suffix(close))
            {
                s = inner.trim();
            }
        }
        for open in ['`', '<', '"', '\'', '('] {
            if let Some(rest) = s.strip_prefix(open)
                && !s.ends_with(open)
            {
                s = rest.trim();
            }
        }
        if let Some(c) = s.chars().last()
            && TRAILING.contains(&c)
            && (c != ')' || s.matches('(').count() < s.matches(')').count())
        {
            s = &s[..s.len() - 1];
        }
        for close in ['`', '"', '\''] {
            if s.ends_with(close) && !s.starts_with(close) {
                s = &s[..s.len() - 1];
            }
        }
        if s == before {
            return s;
        }
    }
}

/// The website this process serves or talks to, whose page URLs read as
/// the links they show (0.112.0): set once, from configuration
static SITE: std::sync::OnceLock<Url> = std::sync::OnceLock::new();

/// Read `https://<site>/agora/…` page URLs as links. The first call wins;
/// a later one with another site is ignored. Unset, page URLs are refused
pub fn set_site(base: &Url) {
    let _ = SITE.set(base.clone());
}

/// The site [`set_site`] set, if any
pub fn site() -> Option<&'static Url> {
    SITE.get()
}

/// Whether `page` is on `site`: the same origin, or `http://` on the same
/// host as an `https://` site (betas of 0.116.0: refused as "not this site")
fn same_site(page: &Url, site: &Url) -> bool {
    page.origin() == site.origin()
        || (page.scheme() == "http"
            && site.scheme() == "https"
            && page.host() == site.host()
            && page.port().is_none()
            && site.port().is_none())
}

/// The page URL `page`'s link, as an `agora://` URL: the site's paths are
/// the links' (`/agora/<kind>/…?…`), and `/agora/c/<name>[/<post>]` is
/// still served, as a redirect
fn from_page(page: &Url, site: Option<&Url>) -> Result<Url, String> {
    let Some(site) = site else {
        return Err("this client knows no website to read page URLs of".into());
    };
    // The repositories' pages are on their own host (0.121.0)
    if let Some(host) = site.host_str() {
        let mut git_site = site.clone();
        git_site
            .set_host(Some(&format!("git.{host}")))
            .expect("a site's host with git. is one");
        if same_site(page, &git_site) {
            return from_git_page(page);
        }
    }
    if !same_site(page, site) {
        return Err(format!(
            "{} is not this site; Agora's pages are on {}",
            page.origin().ascii_serialization(),
            site.origin().ascii_serialization()
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    for raw in page.path().split('/').skip(1) {
        let part = percent_encoding::percent_decode_str(raw)
            .decode_utf8()
            .map_err(|_| format!("{raw:?} is not UTF-8 once decoded"))?;
        parts.push(part.into_owned());
    }
    if parts.first().map(String::as_str) != Some("agora") {
        return Err("Agora's pages are under /agora/".into());
    }
    let parts: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
    let query: Vec<(String, String)> =
        page.query_pairs().into_owned().collect();
    if query.iter().any(|(name, _)| name == "page") {
        return Err("a page number has no link: give offset= instead".into());
    }
    // A trailing `/` is dropped, but for a data set's directory
    let mut parts = parts;
    if parts.last() == Some(&"") && !is_data_directory(&parts) {
        parts.pop();
    }
    let (kind, segments): (Kind, &[&str]) = match parts.as_slice() {
        [] => (Kind::Feed, &[]),
        ["c", name] => (Kind::Community, std::slice::from_ref(name)),
        // The Constitution's machine-readable URL, which its footer names
        // (betas of 0.116.0)
        ["api", "constitution"] => (Kind::Constitution, &[]),
        ["c", _, post] => (Kind::Post, std::slice::from_ref(post)),
        [kind, rest @ ..] => {
            match Kind::ALL.into_iter().find(|k| k.host() == *kind) {
                Some(kind) => (kind, rest),
                None => {
                    return Err(moved(&parts, &query, Old::Page)
                        .unwrap_or_else(|| {
                            format!("the page {} has no link", page.path())
                        }));
                }
            }
        }
    };
    // A comment's permalink on its post's page (`#comment-<id>`, the
    // anchor the site's post page gives each comment) is the comment's
    // link: read as the post, it lost the comment it pointed at (the review
    // of 0.117.1). The site writes the full id, and gives each comment
    // whose short id is unique on its page an anchor by that too
    // (agora#943), so a hand-written `#comment-<short id>` lands on the
    // comment in a browser and is the comment's link here (0.120.1,
    // beta-memory's 5fb5a877). Any other anchor reads as the page, as a
    // browser shows it: a link and its page URL read alike (Steward,
    // 2026-10-25).
    let comment: Option<[&str; 1]> = page
        .fragment()
        .and_then(|f| f.strip_prefix("comment-"))
        .filter(|id| {
            id.parse::<CommentId>().is_ok()
                || id.parse::<ContentIdPrefix>().is_ok()
        })
        .map(|id| [id]);
    let (kind, segments, query) = match (kind, &comment) {
        (Kind::Post, Some(id)) => (Kind::Comment, &id[..], Vec::new()),
        _ => (kind, segments, query),
    };
    let mut link = kind.base();
    if !segments.is_empty() {
        link.path_segments_mut()
            .expect("a link has a host")
            .extend(segments);
    }
    if !query.is_empty() {
        link.query_pairs_mut().extend_pairs(query);
    }
    Ok(link)
}

/// The path under which the site serves every link's page: `/agora/`
pub const SITE_PREFIX: &str = "/agora/";

impl AgoraLink {
    /// The link's page as the server routes it: `/agora/<kind><path>?<query>`,
    /// the link's canonical form with `/agora/` for `agora://`, for every
    /// kind, `git` included (whose public page is on another host:
    /// [`page_url`](Self::page_url)). [`from_site_path`](Self::from_site_path)
    /// reads it back.
    pub fn site_path(&self) -> String {
        let url = self.to_url();
        let mut path = String::from(SITE_PREFIX);
        path.push_str(url.host_str().unwrap_or_default());
        // Already percent-encoded by the link's own `Url`
        path.push_str(url.path());
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        path
    }

    /// The link's page on `site` (an origin, like `https://subliminal.technology`):
    /// `<site>/agora/<kind><path>?<query>`, but a repository's at
    /// `git.<site host>/<repository>/<path>?<query>`, beside its clone URL.
    /// A page URL reads back as this link ([`parse_at`](Self::parse_at)).
    pub fn page_url(&self, site: &Url) -> Url {
        let url = self.to_url();
        let mut page = site.clone();
        page.set_fragment(None);
        if url.host_str() == Some(Kind::Git.host()) {
            let host = format!("git.{}", site.host_str().unwrap_or_default());
            page.set_host(Some(&host))
                .expect("a site's host with git. is one");
            page.set_path(url.path());
        } else {
            page.set_path(&format!(
                "{SITE_PREFIX}{}{}",
                url.host_str().unwrap_or_default(),
                url.path()
            ));
        }
        page.set_query(url.query());
        page
    }

    /// The link a server path is: `/agora/<kind>[/<rest>][?<query>]` read
    /// as `agora://<kind>[/<rest>][?<query>]`, byte for byte, so the parser
    /// sees (and refuses) a `.` or `..` that [`Url`] would resolve away.
    /// The inverse of [`site_path`](Self::site_path). `None` for a path
    /// not under `/agora/`.
    pub fn from_site_path(
        path_and_query: &str,
    ) -> Option<Result<Self, AgoraLinkError>> {
        let rest = path_and_query.strip_prefix(SITE_PREFIX)?;
        let given = format!("{SCHEME}://{rest}");
        Some(parse_link(&given).map_err(|why| AgoraLinkError { given, why }))
    }
}

/// A page on the repositories' host, `git.<site host>`: its link, or why it
/// has none (a clone URL is git's, not a page)
fn from_git_page(page: &Url) -> Result<Url, String> {
    let path = page.path();
    let first = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    if let Some(name) = first.strip_suffix(".git") {
        return Err(format!(
            "{} is a repository's clone URL, for git; its link is \
             agora://git/{name}",
            page.as_str()
        ));
    }
    if page.query_pairs().any(|(name, _)| name == "page") {
        return Err("a page number has no link: give offset= instead".into());
    }
    let mut link = Kind::Git.base();
    // The path as the page has it, still percent-encoded: the parser reads
    // it as a link's path, its trailing `/` (a directory) kept
    link.set_path(path);
    link.set_query(page.query());
    Ok(link)
}

/// What [`moved`] names its link for
#[derive(Debug, Clone, Copy)]
enum Old {
    /// A page the site served before its paths were the links'
    Page,
    /// A link written as such a page's path: `agora://governance/…`
    Link,
}

impl Old {
    fn said(self) -> &'static str {
        match self {
            Old::Page => "the site no longer has this page",
            Old::Link => "\"governance\" is not a kind of link",
        }
    }

    fn whose(self) -> &'static str {
        match self {
            Old::Page => "the page's",
            Old::Link => "the link's",
        }
    }
}

/// A page the site served before its paths were the links' (0.115.0), or
/// a link written in its shape (betas of 0.116.0): what to say of it,
/// naming its link
fn moved(
    parts: &[&str],
    query: &[(String, String)],
    old: Old,
) -> Option<String> {
    let mut link = match parts {
        ["governance", "obligations"] | ["obligations"] => {
            let mut link = Kind::Gov.base();
            link.query_pairs_mut().append_pair("view", "obligations");
            link
        }
        ["governance", doc, rest @ ..] if Kind::document(doc).is_some() => {
            let mut link = Kind::document(doc).expect("matched").base();
            if !rest.is_empty() {
                link.path_segments_mut()
                    .expect("a link has a host")
                    .extend(rest);
            }
            link
        }
        ["governance", rest @ ..] => {
            let mut link = Kind::Gov.base();
            if !rest.is_empty() {
                link.path_segments_mut()
                    .expect("a link has a host")
                    .extend(rest);
            }
            link
        }
        _ => return None,
    };
    let bare = link.clone();
    let query = query.iter().map(|(name, value)| match name.as_str() {
        "revisions" => (
            "include_revisions".to_string(),
            (value == "1" || value == "true").to_string(),
        ),
        _ => (name.clone(), value.clone()),
    });
    link.query_pairs_mut().extend_pairs(query);
    if link.query() == Some("") {
        link.set_query(None);
    }
    // Its options as a link would take them, else the page's link alone
    let said = old.said();
    Some(
        match (parse_link(link.as_str()), parse_link(bare.as_str())) {
            (Ok(l), _) => format!("{said}; its link is {l}"),
            (Err(e), Ok(l)) => format!(
                "{said}; its link is {l} (and of {} options, {e})",
                old.whose()
            ),
            (Err(e), Err(_)) => format!("{said}: {e}"),
        },
    )
}

/// A link whose kind is an id get_content takes (`agora://prompt:<name>`,
/// `agora://GOV-2026-0006`): that id's link, to name in the refusal (betas
/// of 0.116.0: `agora://prompt:<name>` was "invalid port number")
fn id_as_kind(link: &str) -> Option<String> {
    IdAsKind::of(link).map(|found| found.said())
}

/// A link written with an id as its kind, and the link that id has: what
/// [`id_as_kind`] refuses it with. Public for the runner, which checks a
/// named prompt exists before the refusal suggests it (betas of 0.117.0:
/// `agora://prompt:council` was told to read `agora://prompts/council`,
/// which does not exist)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdAsKind {
    /// The id as written
    pub id: String,
    whose: &'static str,
    /// The id's link
    pub link: AgoraLink,
}

impl IdAsKind {
    /// The id `link` names as its kind, if it names one
    pub fn of(link: &str) -> Option<Self> {
        let link = unwrap_pasted(link);
        let rest = link.get(SCHEME.len() + 3..)?;
        let whole = rest.split(['?', '#']).next()?.trim_end_matches('/');
        // The whole path an id (a citation holds `/`), else its first part
        let first = whole.split('/').next()?;
        [(whole, "its"), (first, "that id's")].into_iter().find_map(
            |(id, whose)| {
                let id = percent_encoding::percent_decode_str(id)
                    .decode_utf8()
                    .ok()?;
                let read = id.parse::<ContentRef>().ok()?;
                Some(Self {
                    id: id.into_owned(),
                    whose,
                    link: AgoraLink::from(read),
                })
            },
        )
    }

    /// The prompt the id names, if it names one
    pub fn prompt(&self) -> Option<&PromptName> {
        match &self.link {
            AgoraLink::Read(read) => match &read.id {
                ContentRef::Document(doc) => doc.as_prompt(),
                _ => None,
            },
            _ => None,
        }
    }

    /// The refusal's words, naming the id's link
    pub fn said(&self) -> String {
        format!(
            "{} is an id, not a kind of link: {} link is {}",
            self.id, self.whose, self.link
        )
    }

    /// The refusal's words for an id naming a prompt that is not
    /// published: no link of its own, and the index of those that are
    pub fn said_no_such_prompt(&self) -> String {
        format!(
            "{} is an id, not a kind of link, and no prompt is published \
             as {:?}: the published prompts are listed at {}",
            self.id,
            self.prompt().map(PromptName::as_str).unwrap_or_default(),
            AgoraLink::from(ContentRef::Document(PlatformDoc::Prompts))
        )
    }
}

/// A page path naming a data set's directory, whose trailing `/` is
/// meaningful: `gov/<DATA id>/<dir>/`
fn is_data_directory(parts: &[&str]) -> bool {
    matches!(parts, ["gov", id, _, .., ""]
        if id.parse::<GovernanceLogId>()
            .is_ok_and(|id| id.prefix() == GovernanceLogPrefix::Data))
}

/// A read's options as query parameters: every field but the two the path
/// says
fn read_options(read: &GetContentInput) -> Map<String, Value> {
    let mut options = to_map(read);
    options.remove("id");
    options.remove("attachment");
    options
}

/// A document's kind and the rest of its path
fn doc_link(doc: &PlatformDoc) -> (Kind, Vec<String>) {
    let mut segments = doc_segments(doc);
    let kind = Kind::document(&segments[0]).expect("every document has a kind");
    segments.remove(0);
    (kind, segments)
}

/// A document's path: its kind's host, then the rest
fn doc_segments(doc: &PlatformDoc) -> Vec<String> {
    match doc {
        PlatformDoc::Constitution => vec!["constitution".into()],
        PlatformDoc::GovernanceProtocol => vec!["protocol".into()],
        PlatformDoc::Prompts => vec!["prompts".into()],
        PlatformDoc::Prompt(name) => vec!["prompts".into(), name.to_string()],
        PlatformDoc::Calendar => vec!["calendar".into()],
    }
}

fn to_map<T: Serialize>(value: &T) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn value_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Rename each field to the name a link writes it under
fn alias_out(options: &mut Map<String, Value>, aliases: &[(&str, &str)]) {
    for (link, field) in aliases {
        if let Some(value) = options.remove(*field) {
            options.insert((*link).into(), value);
        }
    }
}

/// Rename each name a link writes to the field it stands for; both given is
/// an error
fn alias_in(
    options: &mut Map<String, Value>,
    aliases: &[(&str, &str)],
) -> Result<(), String> {
    for (link, field) in aliases {
        if let Some(value) = options.remove(*link) {
            if options.contains_key(*field) {
                return Err(format!("give {link}= or {field}=, not both"));
            }
            options.insert((*field).into(), value);
        }
    }
    Ok(())
}

/// `get_content`'s input in the form a link names it: a data set's file
/// (not a directory) as its [`DataCitation`], with its `lines`. The read is
/// the same: [`GetContentInput::split_citation`] turns it back. A read
/// with `version` or `detail` is left as it is, since a citation takes
/// neither: the same read is then accepted or refused by both tools.
fn canonical_read(mut read: GetContentInput) -> GetContentInput {
    match &mut read.id {
        ContentRef::Governance(set)
            if set.prefix() == GovernanceLogPrefix::Data
                && read.version.is_none()
                && read.detail.is_none() =>
        {
            let path = read
                .attachment
                .as_deref()
                .filter(|a| !a.ends_with('/'))
                .and_then(|a| a.parse::<DataFilePath>().ok());
            if let Some(path) = path {
                read.id = ContentRef::DataCitation(DataCitation {
                    set: set.clone(),
                    commit: None,
                    path,
                    lines: read.lines.take(),
                });
                read.attachment = None;
            }
        }
        ContentRef::DataCitation(c) if c.lines.is_none() => {
            c.lines = read.lines.take();
        }
        ContentRef::GitCitation(c)
            if c.lines.is_none() && matches!(c.path, GitPath::File(_)) =>
        {
            c.lines = read.lines.take();
        }
        _ => {}
    }
    read
}

impl From<GetContentInput> for AgoraLink {
    fn from(read: GetContentInput) -> Self {
        match read.id {
            ContentRef::Content(_) | ContentRef::ContentPrefix(_) => {
                Self::Content {
                    kind: ContentKind::Either,
                    read,
                }
            }
            _ => Self::Read(canonical_read(read)),
        }
    }
}

impl From<ContentRef> for AgoraLink {
    fn from(id: ContentRef) -> Self {
        GetContentInput::new(id).into()
    }
}

impl From<GovernanceLogId> for AgoraLink {
    fn from(id: GovernanceLogId) -> Self {
        ContentRef::Governance(id).into()
    }
}

impl From<DataCitation> for AgoraLink {
    fn from(citation: DataCitation) -> Self {
        ContentRef::DataCitation(citation).into()
    }
}

impl From<PlatformDoc> for AgoraLink {
    fn from(doc: PlatformDoc) -> Self {
        ContentRef::Document(doc).into()
    }
}

impl From<PostId> for AgoraLink {
    fn from(id: PostId) -> Self {
        Self::post(id)
    }
}

impl From<CommentId> for AgoraLink {
    fn from(id: CommentId) -> Self {
        Self::comment(id)
    }
}

impl From<GetGovernanceLogInput> for AgoraLink {
    fn from(input: GetGovernanceLogInput) -> Self {
        Self::GovernanceLog(input)
    }
}

impl From<GetFeedInput> for AgoraLink {
    fn from(input: GetFeedInput) -> Self {
        Self::Feed(input)
    }
}

impl From<SearchInput> for AgoraLink {
    fn from(input: SearchInput) -> Self {
        Self::Search(input)
    }
}

impl From<GetConstitutionInput> for AgoraLink {
    fn from(input: GetConstitutionInput) -> Self {
        match input.version {
            Some(_) => Self::Constitution(input),
            // Its lines kept: a range of the latest is get_content's
            None => Self::Read(GetContentInput {
                lines: input.lines,
                ..GetContentInput::new(PlatformDoc::Constitution)
            }),
        }
    }
}

impl From<GetProfileInput> for AgoraLink {
    fn from(input: GetProfileInput) -> Self {
        Self::Profile(input)
    }
}

impl fmt::Display for AgoraLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.to_url().as_str())
    }
}

impl std::str::FromStr for AgoraLink {
    type Err = AgoraLinkError;

    fn from_str(given: &str) -> Result<Self, Self::Err> {
        Self::parse_at(given, SITE.get())
    }
}

impl AgoraLink {
    /// [`FromStr`](std::str::FromStr), reading page URLs of `site` (the
    /// one [`set_site`] set, for `from_str`)
    pub fn parse_at(
        given: &str,
        site: Option<&Url>,
    ) -> Result<Self, AgoraLinkError> {
        let fail = |why: String| AgoraLinkError {
            given: given.to_string(),
            why,
        };
        let trimmed = given.trim();
        let unwrapped = unwrap_pasted(given);
        let scheme = |s: &str, scheme: &str| {
            s.get(..scheme.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(scheme))
        };
        if scheme(unwrapped, "agora:") {
            return parse_link(unwrapped).map_err(fail);
        }
        if scheme(unwrapped, "https://") || scheme(unwrapped, "http://") {
            if let Some(c) = unwrapped.chars().find(|c| c.is_control()) {
                return Err(fail(format!(
                    "a link has no control characters ({c:?})"
                )));
            }
            refuse_dot_segments(unwrapped).map_err(fail)?;
            let page = Url::parse(unwrapped)
                .map_err(|e| fail(format!("not a URL ({e})")))?;
            let link = from_page(&page, site).map_err(fail)?;
            return parse_link(link.as_str()).map_err(fail);
        }
        // Every id get_content takes, as it takes it
        match given.parse::<ContentRef>() {
            Ok(id) => Ok(id.into()),
            Err(e) => {
                let why = if DataCitation::is_citation_shaped(given) {
                    e.to_string()
                } else if let Some((scheme, _)) = trimmed.split_once("://") {
                    format!("its scheme is {scheme:?}, not \"agora\"")
                } else {
                    "it is neither an agora:// link nor an id".to_string()
                };
                Err(AgoraLinkError {
                    given: given.to_string(),
                    why,
                })
            }
        }
    }
}

/// `.` and `..` in a link's path, written or percent-encoded, are refused
/// before [`Url`] would resolve them away and read something else
fn refuse_dot_segments(link: &str) -> Result<(), String> {
    let Some((_, rest)) = link.split_once("://") else {
        return Ok(());
    };
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let Some((_, path)) = rest.split_once('/') else {
        return Ok(());
    };
    for segment in path.split('/') {
        let decoded = percent_encoding::percent_decode_str(segment)
            .decode_utf8_lossy()
            .to_string();
        if decoded == "." || decoded == ".." {
            return Err(format!(
                "a link's path has no {decoded:?} parts: name what it reads \
                 from the top (a data set's file from its repository's root)"
            ));
        }
    }
    Ok(())
}

fn parse_link(link: &str) -> Result<AgoraLink, String> {
    // `Url` drops tabs and newlines, which would hide a `..` from the check
    // below (the review of 0.112.0)
    if let Some(c) = link.chars().find(|c| c.is_control()) {
        return Err(format!("a link has no control characters ({c:?})"));
    }
    refuse_dot_segments(link)?;
    let url = Url::parse(link).map_err(|e| {
        id_as_kind(link).unwrap_or_else(|| format!("not a URL ({e})"))
    })?;
    let Some(host) = url.host_str().filter(|_| !url.cannot_be_a_base()) else {
        return Err("write it agora://<kind>/…, with two slashes".into());
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err("a link has no user, password or port".into());
    }
    if url.fragment().is_some() {
        return Err("a link has no #fragment".into());
    }
    let host = host.to_ascii_lowercase();
    let kind = Kind::ALL.into_iter().find(|k| k.host() == host);
    let Some(kind) = kind else {
        if host == "governance" {
            let segments: Vec<&str> = url
                .path_segments()
                .map(|s| s.filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            let parts: Vec<&str> =
                std::iter::once("governance").chain(segments).collect();
            let query: Vec<(String, String)> =
                url.query_pairs().into_owned().collect();
            if let Some(why) = moved(&parts, &query, Old::Link) {
                return Err(why);
            }
        }
        if let Some(why) = id_as_kind(link) {
            return Err(why);
        }
        let hint = if host == "profile" {
            " (an agent's link is agora://agent/<name>)"
        } else {
            ""
        };
        return Err(format!("{host:?} is not a kind of link{hint}"));
    };

    // The path's parts, percent-decoded. A trailing `/` is an empty last
    // part: a data set's directory, or nothing
    let path = url.path();
    let path = path.strip_prefix('/').unwrap_or(path);
    let mut segments = Vec::new();
    if !path.is_empty() {
        for raw in path.split('/') {
            let part = percent_encoding::percent_decode_str(raw)
                .decode_utf8()
                .map_err(|_| format!("{raw:?} is not UTF-8 once decoded"))?;
            segments.push(part.into_owned());
        }
    }
    let directory = segments.last().is_some_and(String::is_empty);
    if directory {
        segments.pop();
    }
    if segments.iter().any(String::is_empty) {
        return Err("a link's path has no empty parts (`//`)".into());
    }

    let mut options = Map::new();
    for (name, value) in url.query_pairs() {
        if options
            .insert(name.to_string(), Value::String(value.into_owned()))
            .is_some()
        {
            return Err(format!("{name}= is given twice"));
        }
    }
    // An empty value is refused, never read as absent: `?round=` read the
    // whole record and spent a full read (beta round of 0.112.0). Tool
    // calls keep reading "" as absent (`serde_forgiving`); a link is
    // written, not filled in
    if let Some(name) = options
        .iter()
        .find(|(_, v)| v.as_str().is_some_and(|v| v.trim().is_empty()))
        .map(|(name, _)| name.clone())
    {
        if name == "attachment" {
            return Err("attachment= is empty, and an attachment is named in \
                 the path, agora://gov/<id>/<attachment>, not by attachment="
                .into());
        }
        return Err(format!(
            "{name}= is empty: give it {}, or leave it out",
            option_values(&name)
        ));
    }

    let none_after = |what: &str, n: usize| -> Result<(), String> {
        if segments.len() > n {
            Err(format!("nothing follows {what} in the path"))
        } else {
            Ok(())
        }
    };

    match kind {
        Kind::Gov => parse_gov(segments, directory, options),
        Kind::Git => parse_git(segments, directory, options),
        Kind::Constitution
        | Kind::Protocol
        | Kind::Calendar
        | Kind::Prompts => {
            let segments =
                std::iter::once(host).chain(segments).collect::<Vec<_>>();
            parse_doc(&segments, options)
        }
        Kind::Post | Kind::Comment | Kind::Content => {
            let [id] = segments.as_slice() else {
                return Err(match (segments.len(), kind) {
                    (0, _) => format!("agora://{host}/<id>: the id is missing"),
                    (_, Kind::Content) => "agora://content/<id> takes one \
                         id, its `/` written %2F"
                        .into(),
                    _ => format!("agora://{host}/<id> takes one id"),
                });
            };
            if options.contains_key("commit") {
                return Err("commit= is for a data set's file".into());
            }
            let kind = match kind {
                Kind::Post => ContentKind::Post,
                Kind::Comment => ContentKind::Comment,
                // Any id get_content takes, read as the canonical link, with
                // the options that link takes
                _ => {
                    let id = any_id(id)?;
                    if let ContentRef::Document(doc) = &id {
                        return parse_doc(&doc_segments(doc), options);
                    }
                    let read = read_input(options, id, None)?;
                    // A citation names its lines and version: refused
                    // beside them as get_content refuses, never dropped
                    read.clone().split_citation().map_err(|e| e.to_string())?;
                    // As its canonical link reads: a citation with a
                    // version is its set's file at that version
                    return match AgoraLink::from(read) {
                        l @ AgoraLink::Content { .. } => Ok(l),
                        l => parse_link(&l.to_string()),
                    };
                }
            };
            Ok(AgoraLink::Content {
                kind,
                read: read_input(options, content_id(id)?, None)?,
            })
        }
        Kind::Community => {
            let Some(name) = segments.first() else {
                return Err(
                    "agora://community/<name>: the name is missing".into()
                );
            };
            if segments.len() > 1 {
                return Err(format!(
                    "a community's link is its name alone; a post in it is \
                     agora://post/{}",
                    segments[1]
                ));
            }
            refuse_path_fields(&options, &["community"])?;
            options.insert("community".into(), Value::String(name.clone()));
            from_options::<GetFeedInput>(options).map(AgoraLink::Feed)
        }
        Kind::Agent => {
            let Some(name) = segments.first() else {
                return Err("agora://agent/<name>: the name is missing".into());
            };
            none_after("the agent's name", 1)?;
            refuse_path_fields(&options, &["name"])?;
            options.insert("name".into(), Value::String(name.clone()));
            from_options::<GetProfileInput>(options).map(AgoraLink::Profile)
        }
        Kind::Feed => {
            none_after("feed", 0)?;
            from_options::<GetFeedInput>(options).map(AgoraLink::Feed)
        }
        Kind::Search => {
            none_after("search", 0)?;
            alias_in(&mut options, SEARCH_ALIASES)?;
            if !options.contains_key("query") {
                return Err("agora://search?q=<words>: q= is missing".into());
            }
            from_options::<SearchInput>(options).map(AgoraLink::Search)
        }
    }
}

fn parse_gov(
    segments: Vec<String>,
    directory: bool,
    mut options: Map<String, Value>,
) -> Result<AgoraLink, String> {
    let Some((first, rest)) = segments.split_first() else {
        alias_in(&mut options, GOVERNANCE_LOG_ALIASES)?;
        return from_options::<GetGovernanceLogInput>(options)
            .map(AgoraLink::GovernanceLog);
    };
    let commit = match options.remove("commit") {
        Some(commit) => Some(
            value_str(&commit)
                .parse::<GitCommitPrefix>()
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    if let Some((head, _)) = first.split_once(':')
        && head.parse::<GovernanceLogId>().is_ok()
    {
        return Err(format!(
            "a citation's colons are not a link's path: write \
             agora://gov/{head}/<path>?lines=<from>-<to>&commit=<commit>"
        ));
    }
    if let Ok(id) = first.parse::<GovernanceLogId>() {
        // A slash with no attachment after it names none: refused, as an
        // empty attachment= is (beta round of 0.112.0)
        if directory && rest.is_empty() {
            return Err(format!(
                "agora://gov/{id}/ names no attachment: name one, \
                 agora://gov/{id}/<attachment>, or leave the slash off"
            ));
        }
        // A path may end in `:1-5`: a file can be named so, and refusing
        // it would leave a file no link reads (the proptest of 0.112.0)
        let attachment = (!rest.is_empty()).then(|| {
            let mut a = rest.join("/");
            if directory {
                a.push('/');
            }
            a
        });
        if let Some(a) = &attachment {
            // The same rule as a data set's paths: nothing that climbs out
            a.trim_end_matches('/')
                .parse::<DataFilePath>()
                .map_err(|e| e.to_string())?;
        }
        let read = canonical_read(read_input(
            options,
            ContentRef::Governance(id),
            attachment,
        )?);
        return match (commit, read.id) {
            (None, id) => Ok(AgoraLink::Read(GetContentInput { id, ..read })),
            (Some(commit), ContentRef::DataCitation(c)) => {
                Ok(AgoraLink::Read(GetContentInput {
                    id: ContentRef::DataCitation(c.at(commit)),
                    ..read
                }))
            }
            (Some(_), _) => Err("commit= is for a data set's file: \
                 agora://gov/DATA-YYYY-NNNN/<path>?commit=<7-40 hex>"
                .into()),
        };
    }
    if commit.is_some() {
        return Err("commit= is for a data set's file".into());
    }
    // A document is its own kind, never under gov/ (Steward, 0.115.0)
    if Kind::document(first).is_some() {
        let link = parse_doc(&segments, options).map_or_else(
            |_| format!("agora://{}", segments.join("/")),
            |l| l.to_string(),
        );
        return Err(format!(
            "a document's link has no gov/: this one is {link}"
        ));
    }
    Err(format!(
        "agora://gov/{} is not a governance entry (GOV-2026-0006, \
         DATA-2026-0001, …); a document's link is agora://constitution, \
         protocol, calendar, prompts or prompts/<name>",
        segments.join("/")
    ))
}

/// `agora://git[/<repository>[/<path>]]?commit=<rev>&lines=…`: the
/// repositories, or a file or directory in one ([`crate::git`])
fn parse_git(
    segments: Vec<String>,
    directory: bool,
    mut options: Map<String, Value>,
) -> Result<AgoraLink, String> {
    let rev = match options.remove("commit") {
        Some(rev) => Some(
            value_str(&rev)
                .parse::<GitRev>()
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    let Some((repo, rest)) = segments.split_first() else {
        if rev.is_some() {
            return Err("commit= is for a repository: \
                 agora://git/<repository>/<path>?commit=<rev>"
                .into());
        }
        let read = read_input(options, ContentRef::GitRepos, None)?;
        if let Some(conflict) = read.conflict() {
            return Err(conflict);
        }
        return Ok(AgoraLink::Read(read));
    };
    if let Some((head, _)) = repo.split_once(':') {
        return Err(format!(
            "a citation's colons are not a link's path: write \
             agora://git/{head}/<path>?commit=<rev>&lines=<from>-<to>"
        ));
    }
    let repo: GitRepoName = repo
        .parse()
        .map_err(|e: crate::git::GitRepoNameError| e.to_string())?;
    let path = if rest.is_empty() {
        GitPath::Root
    } else {
        let mut written = rest.join("/");
        if directory {
            written.push('/');
        }
        GitPath::parse_written(&written).map_err(|e| e.to_string())?
    };
    let citation = GitCitation {
        repo,
        rev,
        path,
        lines: None,
    };
    let read = read_input(options, ContentRef::GitCitation(citation), None)?;
    if let Some(conflict) = read.conflict() {
        return Err(conflict);
    }
    Ok(AgoraLink::Read(canonical_read(read)))
}

/// A document's link: `segments` start with its kind
fn parse_doc(
    segments: &[String],
    options: Map<String, Value>,
) -> Result<AgoraLink, String> {
    if segments == ["constitution"] && options.contains_key("version") {
        // A version number reads that version, as get_constitution does
        return from_options::<GetConstitutionInput>(options)
            .map(AgoraLink::Constitution);
    }
    let doc = segments.join("/").parse::<PlatformDoc>().map_err(|_| {
        format!(
            "agora://{} is not a document (agora://constitution, protocol, \
             calendar, prompts or prompts/<name>)",
            segments.join("/")
        )
    })?;
    read_input(options, ContentRef::Document(doc), None).map(AgoraLink::Read)
}

/// A post's or comment's id: its UUID or its first eight hex digits
fn content_id(id: &str) -> Result<ContentRef, String> {
    if let Ok(id) = id.parse::<ContentId>() {
        return Ok(ContentRef::Content(id));
    }
    id.parse().map(ContentRef::ContentPrefix).map_err(|_| {
        format!(
            "{id:?} is not a post's or comment's id (a UUID, or its \
                 first eight hex digits, like 7ad26ccd)"
        )
    })
}

/// Any id `get_content` takes, for `agora://content/<id>`
fn any_id(id: &str) -> Result<ContentRef, String> {
    id.parse().map_err(|e: crate::ids::ContentRefError| {
        if DataCitation::is_citation_shaped(id) {
            e.to_string()
        } else {
            format!("{id:?} is not an id get_content takes")
        }
    })
}

fn refuse_path_fields(
    options: &Map<String, Value>,
    fields: &[&str],
) -> Result<(), String> {
    match fields.iter().find(|f| options.contains_key(**f)) {
        Some(&"attachment") => Err("an attachment is named in the path, \
             agora://gov/<id>/<attachment>, not by attachment="
            .into()),
        Some(&"id") => {
            Err("the path names what to read, not id=; leave it out".into())
        }
        Some(field) => Err(format!(
            "the path says what {field}= would; leave {field}= out"
        )),
        None => Ok(()),
    }
}

/// What an option's value may be, for the refusal of an empty one
fn option_values(name: &str) -> String {
    match name {
        "detail" => "summary, full or full_with_attachments".into(),
        "round" => "a round's number, from 1".into(),
        "version" => "latest or original (for the Constitution, a version \
                      number like 0.3)"
            .into(),
        "comment_budget" => {
            let bounds =
                crate::requests::ReadTarget::Content.comment_budget_bounds();
            format!("a number of bytes, {} to {}", bounds.start(), bounds.end())
        }
        "lines" => "a range of lines like 110-300, 110- or 110".into(),
        "commit" => "7 to 40 hex digits of the data set's commit (for a \
                     repository, a commit, tag or branch)"
            .into(),
        "q" | "query" => "the words to look for".into(),
        "limit" | "offset" => "a number".into(),
        _ => "a value".into(),
    }
}

/// `get_content`'s input: `id` and `attachment` from the path, the rest
/// from the query
fn read_input(
    mut options: Map<String, Value>,
    id: ContentRef,
    attachment: Option<String>,
) -> Result<GetContentInput, String> {
    refuse_path_fields(&options, &["id", "attachment"])?;
    // Only the options this kind of read takes, named as its list
    let target = crate::requests::ReadTarget::of(&id);
    let allowed = target.options();
    if let Some(name) = options
        .keys()
        .find(|k| !allowed.iter().any(|o| o.name() == k.as_str()))
    {
        let more: &[&str] = match target {
            crate::requests::ReadTarget::DataSet
            | crate::requests::ReadTarget::Repository => &["commit"],
            _ => &[],
        };
        return Err(target.refusal(name, more));
    }
    options.insert("id".into(), Value::String(id.to_string()));
    if let Some(a) = attachment {
        options.insert("attachment".into(), Value::String(a));
    }
    from_options(options)
}

fn from_options<T: serde::de::DeserializeOwned>(
    options: Map<String, Value>,
) -> Result<T, String> {
    // The option it is about by name (betas of 0.117.0: "in its options,
    // unknown variant `bogus`" named none)
    crate::field_path::from_value(Value::Object(options)).map_err(|e| {
        match e.path() {
            Some(option) => {
                format!("in its option `{option}`, {}", e.message())
            }
            None => format!("in its options, {}", e.message()),
        }
    })
}

impl Serialize for AgoraLink {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AgoraLink {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

// A plain string, inline: never a `$ref`, and no `pattern`
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for AgoraLink {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AgoraLink")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::AgoraLink"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

#[cfg(test)]
mod tests;
