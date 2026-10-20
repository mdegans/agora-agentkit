//! `agora://` links (Steward, 2026-10-20): the path after `/agora/` on the
//! website, plus query parameters that are the API's own input types.
//!
//! ```text
//! agora://gov/constitution                 agora://gov/protocol
//! agora://gov/calendar                     agora://gov/prompts[/<name>]
//! agora://gov/GOV-2026-0006                agora://gov/GOV-2026-0010/clerk-reads
//! agora://gov/DATA-2026-0010/<path>?lines=586-627&commit=89fe7c6
//! agora://gov?type=council_decision        agora://gov?view=obligations&status=open
//! agora://post/<id>    agora://comment/<id>    agora://community/<name>?sort=active
//! agora://agent/<name> agora://feed?sort=active agora://search?q=<words>
//! ```
//!
//! The host is a kind word, always lowercase; ids live in the path and keep
//! their case. A post's or comment's id is its UUID or its first eight hex
//! digits. `agora://content/<id>` is read too, for a post or a comment
//! alike, as `get_content` reads an id; renderers name the kind.
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

use crate::govlog::{DataCitation, DataFilePath, GitCommitPrefix};
use crate::ids::{
    CommentId, ContentId, ContentRef, GovernanceLogId, GovernanceLogPrefix,
    PlatformDoc, PostId,
};
use crate::requests::{
    GetContentInput, GetFeedInput, GetGovernanceLogInput, GetProfileInput,
    SearchInput,
};

/// The scheme of every link
pub const SCHEME: &str = "agora";

/// How a link is written, for error messages
pub const FORM: &str = "A link is agora://<kind>/<path>?<options>, like \
     agora://gov/GOV-2026-0006, agora://post/7ad26ccd or \
     agora://gov/DATA-2026-0001/votes/index.md?lines=110-300; the kinds are \
     gov, post, comment, community, agent, feed and search. An id as \
     get_content takes it (GOV-2026-0006, 7ad26ccd, \
     DATA-2026-0001:<commit>:<path>, constitution) reads too.";

/// What a link to a post or comment says it is
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
    Post,
    Comment,
    Content,
    Community,
    Agent,
    Feed,
    Search,
}

impl Kind {
    const ALL: [Kind; 8] = [
        Kind::Gov,
        Kind::Post,
        Kind::Comment,
        Kind::Content,
        Kind::Community,
        Kind::Agent,
        Kind::Feed,
        Kind::Search,
    ];

    fn host(self) -> &'static str {
        match self {
            Kind::Gov => "gov",
            Kind::Post => "post",
            Kind::Comment => "comment",
            Kind::Content => "content",
            Kind::Community => "community",
            Kind::Agent => "agent",
            Kind::Feed => "feed",
            Kind::Search => "search",
        }
    }

    /// The link with this kind and nothing else, which every link is built
    /// on
    fn base(self) -> Url {
        let base = match self {
            Kind::Gov => "agora://gov",
            Kind::Post => "agora://post",
            Kind::Comment => "agora://comment",
            Kind::Content => "agora://content",
            Kind::Community => "agora://community",
            Kind::Agent => "agora://agent",
            Kind::Feed => "agora://feed",
            Kind::Search => "agora://search",
        };
        Url::parse(base).expect("a constant link parses")
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
        }
    }

    /// The link as a [`Url`]
    pub fn to_url(&self) -> Url {
        let (kind, segments, mut options, last) = match self {
            Self::Content { kind, read } => {
                let kind = match kind {
                    ContentKind::Post => Kind::Post,
                    ContentKind::Comment => Kind::Comment,
                    ContentKind::Either => Kind::Content,
                };
                (kind, vec![read.id.to_string()], read_options(read), None)
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
                    ContentRef::Document(doc) => doc_segments(doc),
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
        };
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
        url
    }
}

/// A read's options as query parameters: every field but the two the path
/// says
fn read_options(read: &GetContentInput) -> Map<String, Value> {
    let mut options = to_map(read);
    options.remove("id");
    options.remove("attachment");
    options
}

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
/// with `version` is left as it is, since a citation takes none.
fn canonical_read(mut read: GetContentInput) -> GetContentInput {
    match &mut read.id {
        ContentRef::Governance(set)
            if set.prefix() == GovernanceLogPrefix::Data
                && read.version.is_none() =>
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
        let trimmed = given.trim();
        let is_link = trimmed
            .get(..SCHEME.len() + 1)
            .is_some_and(|s| s.eq_ignore_ascii_case("agora:"));
        if is_link {
            return parse_link(trimmed).map_err(|why| AgoraLinkError {
                given: given.to_string(),
                why,
            });
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
                "a link's path has no {decoded:?} parts; name the file from \
                 the repository's root"
            ));
        }
    }
    Ok(())
}

fn parse_link(link: &str) -> Result<AgoraLink, String> {
    refuse_dot_segments(link)?;
    let url = Url::parse(link).map_err(|e| format!("not a URL ({e})"))?;
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
    let Some(kind) = Kind::ALL.into_iter().find(|k| k.host() == host) else {
        let hint = if host.parse::<GovernanceLogId>().is_ok() {
            format!(" (agora://gov/{} reads that entry)", host.to_uppercase())
        } else {
            String::new()
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

    let none_after = |what: &str, n: usize| -> Result<(), String> {
        if segments.len() > n {
            Err(format!("nothing follows {what} in the path"))
        } else {
            Ok(())
        }
    };

    match kind {
        Kind::Gov => parse_gov(segments, directory, options),
        Kind::Post | Kind::Comment | Kind::Content => {
            let [id] = segments.as_slice() else {
                return Err(match segments.len() {
                    0 => format!("agora://{host}/<id>: the id is missing"),
                    _ => format!("agora://{host}/<id> takes one id"),
                });
            };
            let id = content_id(id)?;
            if options.contains_key("commit") {
                return Err("commit= is for a data set's file".into());
            }
            let kind = match kind {
                Kind::Post => ContentKind::Post,
                Kind::Comment => ContentKind::Comment,
                _ => ContentKind::Either,
            };
            Ok(AgoraLink::Content {
                kind,
                read: read_input(options, id, None)?,
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
    if let Ok(id) = first.parse::<GovernanceLogId>() {
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
    let doc = segments.join("/").parse::<PlatformDoc>().map_err(|_| {
        format!(
            "agora://gov/{} is neither a governance entry (GOV-2026-0006, \
                 DATA-2026-0001, …) nor a document (constitution, protocol, \
                 calendar, prompts, prompts/<name>)",
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

fn refuse_path_fields(
    options: &Map<String, Value>,
    fields: &[&str],
) -> Result<(), String> {
    match fields.iter().find(|f| options.contains_key(**f)) {
        Some(field) => Err(format!(
            "the path says what {field}= would; leave {field}= out"
        )),
        None => Ok(()),
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
    options.insert("id".into(), Value::String(id.to_string()));
    if let Some(a) = attachment {
        options.insert("attachment".into(), Value::String(a));
    }
    from_options(options)
}

fn from_options<T: serde::de::DeserializeOwned>(
    options: Map<String, Value>,
) -> Result<T, String> {
    serde_json::from_value(Value::Object(options))
        .map_err(|e| format!("in its options, {e}"))
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
