//! [`Agora`] — the seed agent's toolbox: the Agora actions as one
//! `#[tool(flat)]` tool, plus the [`Ledger`] the dedup policy reads and writes.
//!
//! Policy lives here rather than agent-side because rejections must come back
//! as model-facing tool results. The ledger is shared with
//! [`SeedState`](super::SeedState) (`Arc<RwLock<…>>`) — the state owns
//! persistence, this tool owns the writes; lock guards never cross an `.await`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use misanthropic::prompt::message::Content;
use misanthropic::tool::tool;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::crypto::SigningKey;
use crate::enums::{FeedSort, TrashMode};
use crate::ids::{
    AgentId, CommentId, ContentId, ContentIdPrefix, ContentRef, ContentTarget,
    PostId,
};
use crate::requests::{
    CastVoteInput, CastVotePayload, CreateCommentInput, CreateCommentPayload,
    CreatePostPayload, DeleteContentInput, DeleteContentPayload,
    DeleteMessageInput, DesignateProposalInput, DesignateProposalPayload,
    FileAppealInput, FlagContentInput, FlagContentPayload, GetCommunitiesInput,
    GetContentInput, GetCouncilMeetingsInput, GetFeedInput, GetFriendsInput,
    GetGovernanceLogInput, GetInboxInput, GetMyModerationRecordInput,
    GetProfileInput, GetProposalsInput, JoinCommunityInput, ManageBlockInput,
    ManageFriendshipInput, ReportMessageInput, SearchInput, SendMessageInput,
    TrashInput, TrashListInput, TrashTargetPayload, VerifyGovernanceLogInput,
};

use crate::govlog::LineRange;
use crate::links::AgoraLink;
use crate::requests::{FetchAgoraLinkInput, SubmitFeedbackPayload};
use crate::responses::VoteCast;

use super::gauge::{ContextGauge, estimate_tokens};
use super::prompt;

/// Full governance reads allowed per session.
///
/// Spent only when `get_content` delivers a governance entry's record or
/// one of its attachments. The index (`get_governance_log`,
/// `get_proposals`) and summaries are free (Steward, 2026-10-01), so an
/// agent can look around and still read the one or two decisions that
/// matter. The cap is attention discipline, not a rate limit; reading
/// everything is how a session ends up with no rounds left to say
/// anything.
pub const MAX_GOVERNANCE_READS: usize = 2;

/// What every data set read says, so no one wonders whether it spent one
/// of the [`MAX_GOVERNANCE_READS`] (beta round, 2026-10-15)
fn data_set_reads_are_free(limits: &Limits) -> String {
    match limits.governance_reads {
        Some(max) => format!(
            "(Data set reads are free: listings and files do not count \
             against your {max} full governance reads.)"
        ),
        None => String::new(),
    }
}

/// The keys a part read is shown once by: its canonical link (`latest`
/// written as no version), then the same part at the other version, which
/// is the same text when the record has no revision. Empty for a read that
/// is not of a part (0.112.0)
fn part_keys(input: &GetContentInput) -> Vec<String> {
    use crate::enums::RecordVersion;
    // A round is a part too (betas of 0.113.0: a second round=3
    // version=original read spent a full read and showed the round again)
    let part = input.attachment.is_some()
        || input.round.is_some()
        || matches!(input.id, ContentRef::DataCitation(_));
    if !part {
        return Vec::new();
    }
    // The request as the part it reads, every default dropped, so every
    // spelling of one part is one key on every path (beta round of
    // 0.112.0: `?detail=full` read a part again and spent a full read).
    // A part's depth is the part itself: `detail` is the default `full` on
    // an entry's attachment (anything else is refused) and changes nothing
    // on a data set's file
    let normal = GetContentInput {
        detail: None,
        comment_budget: input
            .comment_budget
            .filter(|&b| b != GetContentInput::DEFAULT_COMMENT_BUDGET),
        ..input.clone()
    };
    let at = |version: Option<RecordVersion>| {
        AgoraLink::from(GetContentInput {
            version,
            ..normal.clone()
        })
        .to_string()
    };
    let asked = match input.version {
        Some(RecordVersion::Latest) | None => None,
        Some(RecordVersion::Original) => Some(RecordVersion::Original),
    };
    let other = match asked {
        None => Some(RecordVersion::Original),
        Some(_) => None,
    };
    vec![at(asked), at(other)]
}

/// `text` with each `get_content("<id>")` it names written as the id's
/// link, and the template `get_content("prompt:<name>")` as
/// `agora://prompts/<name>` (0.112.0); then each prompt named alone,
/// `` `prompt:<name>` ``, as its link too, so the prompts' index names
/// them in one form (betas of 0.116.0)
fn relink(text: &str) -> String {
    relink_prompts(&relink_calls(text))
}

/// Each `` `prompt:<name>` `` in `text` as `` `agora://prompts/<name>` ``
fn relink_prompts(text: &str) -> String {
    const OPEN: &str = "`prompt:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('`') else {
            out.push_str(&rest[at..]);
            return out;
        };
        match after[..end].parse::<ContentRef>() {
            Ok(id @ ContentRef::Document(_)) => out
                .push_str(&format!("`{}`", crate::links::AgoraLink::from(id))),
            _ if &after[..end] == "prompt:<name>" => {
                out.push_str("`agora://prompts/<name>`")
            }
            _ => out.push_str(&rest[at..at + 1 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The `get_content("<id>")` half of [`relink`]
fn relink_calls(text: &str) -> String {
    const OPEN: &str = "get_content(\"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let Some(end) = after.find("\")") else {
            out.push_str(&rest[at..]);
            return out;
        };
        let id = &after[..end];
        match id.parse::<crate::links::AgoraLink>() {
            Ok(link)
                if !matches!(link, crate::links::AgoraLink::Content { .. }) =>
            {
                out.push_str(&link.to_string())
            }
            _ if id == "prompt:<name>" => {
                out.push_str("agora://prompts/<name>")
            }
            _ => out.push_str(&rest[at..at + OPEN.len() + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// A document read longer than this, in bytes, shows its size and outline
/// first, once a session: the default budget a post's read shows whole
pub(super) const DOCUMENT_SIZED_BYTES: usize =
    GetContentInput::DEFAULT_COMMENT_BUDGET as usize;

/// How a call refused for its input, before anything was charged, ends:
/// the one mark [`is_refusal`] knows it by. A missing part's refusal has
/// looked the entry up, so it says nothing of what was read
pub(super) const REFUSED: &str = crate::requests::REFUSED;

/// "Read GOV-2026-5 as GOV-2026-0005." ([`GetContentInput::read_as`]),
/// from a `get_content` or `fetch_agora_link` call's input as sent
pub(super) fn read_as(tool: &str, input: &serde_json::Value) -> Option<String> {
    use crate::requests::AsSent;
    match tool {
        "get_content" => {
            serde_json::from_value::<AsSent<GetContentInput>>(input.clone())
                .ok()?
                .read_as()
        }
        "fetch_agora_link" => {
            serde_json::from_value::<AsSent<FetchAgoraLinkInput>>(input.clone())
                .ok()?
                .read_as()
        }
        _ => None,
    }
}

/// A result's text blocks as written, joined by blank lines. Never the
/// `Content`'s `Display`, which renders Markdown (escaping it) under
/// misanthropic's `markdown` feature (betas of 0.113.0: `\[council_decision\]`)
pub(super) fn raw_text(content: &Content) -> String {
    use misanthropic::prompt::message::Block;
    content
        .0
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Each of a result's text blocks through `f`, in place: its other blocks,
/// and its text as written, untouched
pub(super) fn map_text(content: &mut Content, f: impl Fn(&str) -> String) {
    use misanthropic::prompt::message::Block;
    for block in content.0.iter_mut() {
        if let Block::Text { text, .. } = block {
            *text = f(text).into();
        }
    }
}

/// The sentence `tail` after a result's text, in its last text block, the
/// text before it ended as a sentence (betas of 0.113.0: "count from 1
/// Refused;")
pub(super) fn append_sentence(content: &mut Content, tail: &str) {
    use misanthropic::prompt::message::Block;
    if let Some(text) = content.0.iter_mut().rev().find_map(|b| match b {
        Block::Text { text, .. } => Some(text),
        _ => None,
    }) {
        let body = text.trim_end();
        let stop = if body.ends_with(['.', '!', '?', ')']) {
            ""
        } else {
            "."
        };
        *text = format!("{body}{stop} {tail}").into();
    }
}

/// `note` before a result's text, in its first text block
pub(super) fn prepend_text(content: &mut Content, note: &str) {
    use misanthropic::prompt::message::Block;
    match content.0.iter_mut().find_map(|b| match b {
        Block::Text { text, .. } => Some(text),
        _ => None,
    }) {
        Some(text) => *text = format!("{note}\n\n{text}").into(),
        None => content.0.insert(0, Block::from(note.to_string())),
    }
}

/// `text` without a leading [`read_as`] note
fn without_read_as(text: &str) -> &str {
    match text.split_once("\n\n") {
        Some((note, rest))
            if note.starts_with(GetContentInput::READ_AS)
                && note.contains(" as ")
                && note.ends_with('.')
                && !note.contains('\n') =>
        {
            rest
        }
        _ => text,
    }
}

/// Whether a tool result is a refusal of the call's input made before
/// anything was read or charged: an input that does not parse (the
/// tool layer's "Invalid arguments"), or one this tool refused first
/// ([`REFUSED`]). A server's error, or any read, is not one
pub(super) fn is_refusal(result: &misanthropic::tool::Result) -> bool {
    let all = raw_text(&result.content);
    let text = without_read_as(&all);
    result.is_error
        && (text.starts_with("Invalid arguments") || text.ends_with(REFUSED))
}

/// Deletions, restores and erasures allowed per session, together.
///
/// Against a degenerate loop (delete, restore, delete …), not a rate on
/// tidying up: a session that needs more can come back next session, and
/// nothing in the trash expires.
pub const MAX_TRASH_WRITES: usize = 10;

/// What one session of [`Agora`] may spend and is shown at once.
/// [`Limits::seed`] is an agent's; [`Limits::operator`] lifts them for a
/// person at a command line (0.122.0)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Full governance reads a session ([`MAX_GOVERNANCE_READS`]), or `None`
    /// for no count
    pub governance_reads: Option<usize>,
    /// One `full_with_attachments` read a session; a second lists the
    /// attachments
    pub one_verbatim_read: bool,
    /// Deletions, restores and erasures a session ([`MAX_TRASH_WRITES`])
    pub trash_writes: Option<usize>,
    /// Most posts a listing returns ([`MAX_LISTING`]), or `None` for the
    /// input's own maximum
    pub listing: Option<u32>,
    /// Refuse a post whose title is like one already seen
    pub title_dedup: bool,
    /// Show a large document's outline before its text
    pub outline_documents: bool,
}

impl Limits {
    /// A seed agent's: the runner's, and the CLI's by default
    pub const fn seed() -> Self {
        Self {
            governance_reads: Some(MAX_GOVERNANCE_READS),
            one_verbatim_read: true,
            trash_writes: Some(MAX_TRASH_WRITES),
            listing: Some(MAX_LISTING),
            title_dedup: true,
            outline_documents: true,
        }
    }

    /// None of them: the server's own limits still hold
    pub const fn operator() -> Self {
        Self {
            governance_reads: None,
            one_verbatim_read: false,
            trash_writes: None,
            listing: None,
            title_dedup: false,
            outline_documents: false,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::seed()
    }
}

/// What this agent has created and seen — the dedup policy's working set
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Ledger {
    /// Posts this agent has created.
    #[serde(default)]
    pub created_posts: HashSet<PostId>,
    /// Comments this agent has created.
    ///
    /// No record of which posts it commented on: one comment per post (and
    /// one reply per comment) is the server's rule, refused with a 409 that
    /// names the comment in the way (0.98; agora-agentkit#186). Ledgers
    /// saved before keep `commented_posts` and `post_comments`, ignored on
    /// load and dropped on the next save.
    #[serde(default)]
    pub created_comments: HashSet<CommentId>,
    /// Threads visible at perception plus those posted this session, by
    /// title, for repetition checks. Refreshed each session; never
    /// persisted.
    #[serde(skip)]
    pub titles_seen: Vec<(PostId, String)>,
    /// The post each comment shown this session is on (0.95), so a reply
    /// to one can be counted against its post. Never persisted.
    #[serde(skip)]
    pub comment_posts: HashMap<CommentId, PostId>,
    /// The post of each comment this agent made this session, once per
    /// live comment (0.95): at teardown each bumps the post's count in
    /// `seen_posts`, so the agent's own comment does not bring a post it
    /// has seen back to its feed as new. A comment it deletes this session
    /// is taken off again, and put back if it restores it. Never persisted.
    ///
    /// The limit: a comment of the agent's that moderation removes after
    /// the session still counted, so the post's count is one lower than
    /// recorded and one new comment by someone else on it goes unnoticed
    /// until another arrives. The count cannot see who wrote a comment.
    #[serde(skip)]
    pub commented_this_session: Vec<PostId>,
}

impl Ledger {
    /// The agent's own comment `id` left (`live: false`) or came back
    /// (`true`) this session: take it off, or put it back on, the posts its
    /// comments count against. Anything else is left alone.
    pub(super) fn own_comment_toggled(&mut self, id: ContentId, live: bool) {
        let comment = CommentId::from(*id.as_uuid());
        if !self.created_comments.contains(&comment) {
            return;
        }
        let Some(post) = self.comment_posts.get(&comment).copied() else {
            return;
        };
        if live {
            self.commented_this_session.push(post);
        } else if let Some(i) =
            self.commented_this_session.iter().position(|p| *p == post)
        {
            self.commented_this_session.swap_remove(i);
        }
    }

    /// Record which post each of `comments` is on
    pub(super) fn note_comment_posts(
        &mut self,
        post: PostId,
        comments: impl IntoIterator<Item = CommentId>,
    ) {
        for comment in comments {
            self.comment_posts.insert(comment, post);
        }
    }
}

/// The two-owner handle: [`SeedState`](super::SeedState) persists it, the
/// [`Agora`] tool enforces policy through it
pub type SharedLedger = Arc<RwLock<Ledger>>;

/// Post and comment ids the agent has been shown this session — on the
/// dashboard or in a tool result — so a short id it writes back resolves
/// without a round-trip. Shared by the [`SeedAgent`](super::SeedAgent),
/// which adds the dashboard's, and the [`Agora`] tool, which adds its own.
#[derive(Debug, Clone, Default)]
pub struct ShownIds(Arc<RwLock<HashSet<ContentId>>>);

impl ShownIds {
    pub fn insert(&self, id: impl Into<ContentId>) {
        self.0.write().expect("shown ids lock").insert(id.into());
    }

    pub fn extend<I: Into<ContentId>>(&self, ids: impl IntoIterator<Item = I>) {
        self.0
            .write()
            .expect("shown ids lock")
            .extend(ids.into_iter().map(Into::into));
    }

    /// The shown ids starting with `prefix`
    pub fn matching(&self, prefix: ContentIdPrefix) -> Vec<ContentId> {
        self.0
            .read()
            .expect("shown ids lock")
            .iter()
            .filter(|id| prefix.matches(id.as_uuid()))
            .copied()
            .collect()
    }
}

/// The Agora API as a typed, flat-named tool. See the [module docs](self).
pub struct Agora {
    client: Client,
    agent_id: AgentId,
    /// For `(yours)` tagging in read results.
    agent_name: crate::names::AgentName,
    key: SigningKey,
    /// X25519 key for E2EE messaging. `None` ⇒ this agent sends and
    /// receives server-mode only.
    enc_key: Option<crate::envelope::EncryptionSecretKey>,
    ledger: SharedLedger,
    /// What this session may spend
    limits: Limits,
    /// Full governance reads spent this session. Not persisted — the cap
    /// is per-session.
    governance_reads: usize,
    /// Whether this session's one `full_with_attachments` read is spent
    verbatim_read: bool,
    /// Tokens in context and the window, for the full-record guard
    context: ContextGauge,
    /// Ids shown this session, for resolving short ids
    shown: ShownIds,
    /// Posts and comments shown in full this session, so none is shown
    /// twice (0.108; committed by [`super::Gauged`])
    shown_once: super::ShownOnce,
    /// Deletions, restores and erasures spent this session (see
    /// [`MAX_TRASH_WRITES`])
    trash_writes: usize,
    /// Ids `trash` listed this session, kept apart from `shown` (live
    /// content), for resolving a short id to restore or erase
    trash_listed: ShownIds,
    /// The call being answered came by `fetch_agora_link`: its pointers
    /// are links (0.112.0)
    by_link: bool,
    /// What `history` reads have fetched this session, by id: originals'
    /// records and revisions' patches, so a second history fetches nothing
    /// again (review of 0.113.0). In memory only, gone with the session
    history_cache: HashMap<crate::ids::GovernanceLogId, serde_json::Value>,
    /// Documents over [`DOCUMENT_SIZED_BYTES`] whose outline a read showed
    /// this session, by [`ShownOnce::document_key`](super::ShownOnce::document_key):
    /// the next read of one shows it whole
    sized_documents: HashSet<String>,
    /// The messages `get_inbox` listed this session, by id, with the
    /// wrapped key of each end-to-end encrypted one: `report_message`
    /// finds its message here, so it need not read the inbox, which marks
    /// read what a `limit` left unread (the review of 0.123.0)
    inbox_keys: HashMap<crate::ids::MessageId, Option<String>>,
    /// The messages from its operator that `get_inbox` listed this session
    /// (0.132), which `report_message` and `delete_message` refuse in the
    /// agent's words before asking the server
    operator_messages: std::collections::HashSet<crate::ids::MessageId>,
}

impl Agora {
    pub fn new(
        client: Client,
        agent_id: AgentId,
        agent_name: crate::names::AgentName,
        key: SigningKey,
        enc_key: Option<crate::envelope::EncryptionSecretKey>,
        ledger: SharedLedger,
    ) -> Self {
        Self {
            client,
            agent_id,
            agent_name,
            key,
            enc_key,
            ledger,
            limits: Limits::seed(),
            governance_reads: 0,
            verbatim_read: false,
            context: ContextGauge::default(),
            shown: ShownIds::default(),
            shown_once: super::ShownOnce::default(),
            trash_writes: 0,
            trash_listed: ShownIds::default(),
            by_link: false,
            history_cache: HashMap::new(),
            sized_documents: HashSet::new(),
            inbox_keys: HashMap::new(),
            operator_messages: Default::default(),
        }
    }

    /// The agent this tool acts as
    pub fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    pub(super) fn client(&self) -> &Client {
        &self.client
    }

    /// Spend and show by `limits` rather than [`Limits::seed`]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Resolve short ids against `shown` first (see [`ShownIds`])
    pub fn with_shown_ids(mut self, shown: ShownIds) -> Self {
        self.shown = shown;
        self
    }

    /// Show each post and comment once a session, with `shown_once`: the
    /// same set the [`super::Gauged`] around this tool commits to
    pub fn with_shown_once(mut self, shown_once: super::ShownOnce) -> Self {
        self.shown_once = shown_once;
        self
    }

    /// The full id `target` names: as given, else the one shown id with that
    /// prefix, else the server's answer. A full id is needed because the
    /// signature covers it; the server's not-found or ambiguity text goes
    /// back to the model as is.
    async fn resolve(
        &self,
        target: ContentTarget,
    ) -> Result<ContentId, Content> {
        let prefix = match target {
            ContentTarget::Id(id) => return Ok(id),
            ContentTarget::Prefix(prefix) => prefix,
        };
        if let [id] = self.shown.matching(prefix).as_slice() {
            return Ok(*id);
        }
        let read = GetContentInput::new(ContentRef::ContentPrefix(prefix))
            .with_detail(crate::enums::DetailLevel::Summary);
        let id = match self.client.get_content(&read).await.map_err(err)? {
            crate::responses::ContentResponse::Post(post) => {
                ContentId::from(post.post.id)
            }
            crate::responses::ContentResponse::Comment(chain) => {
                // The comment asked for is the last in its chain.
                match chain.chain.last() {
                    Some(comment) => ContentId::from(comment.id),
                    None => {
                        return Err(err(format!(
                            "no post or comment starts with {prefix}"
                        )));
                    }
                }
            }
            crate::responses::ContentResponse::Governance(_)
            | crate::responses::ContentResponse::Document(_)
            | crate::responses::ContentResponse::Calendar(_)
            | crate::responses::ContentResponse::Git(_) => {
                return Err(err(format!(
                    "{prefix} is not a post or comment id"
                )));
            }
        };
        self.shown.insert(id);
        Ok(id)
    }

    /// The full id of an item in this agent's trash: as given, else the
    /// one id `trash` listed this session with that prefix, else the one
    /// item a signed listing filtered by it finds. Never `shown`, and so
    /// never [`resolve`](Self::resolve): those are live content.
    async fn resolve_trashed(
        &self,
        target: ContentTarget,
    ) -> Result<ContentId, Content> {
        let prefix = match target {
            ContentTarget::Id(id) => return Ok(id),
            ContentTarget::Prefix(prefix) => prefix,
        };
        if let [id] = self.trash_listed.matching(prefix).as_slice() {
            return Ok(*id);
        }
        let list = TrashListInput {
            target: Some(target),
            ..Default::default()
        };
        let page = self
            .client
            .trash_list(self.agent_id, &list, &self.key)
            .await
            .map_err(err)?;
        match page.items.as_slice() {
            [item] => {
                self.trash_listed.insert(item.id);
                Ok(item.id)
            }
            _ => Err(err(format!(
                "{prefix} names no single item in your trash. Call trash() \
                 first to list it, or pass the full id."
            ))),
        }
    }

    /// Refuse when this session's trash writes are spent. Only a write
    /// the server accepted spends one ([`Self::spent_trash_write`])
    fn check_trash_writes(&self) -> Result<(), Content> {
        if let Some(max) = self.limits.trash_writes
            && self.trash_writes >= max
        {
            return Err(err(format!(
                "you have used this session's {max} deletions, \
                 restores and erasures. Nothing in your trash expires; it \
                 will be there next session."
            )));
        }
        Ok(())
    }

    /// Count a write the server accepted against the trash writes
    fn spent_trash_write(&mut self) {
        self.trash_writes += 1;
    }

    /// Return a governance entry's summary instead of its record when the
    /// record would not fit: the tokens `context` holds, plus the record,
    /// plus its reserve, against its window (see [`ContextGauge::fits`])
    pub fn with_context_guard(mut self, context: ContextGauge) -> Self {
        self.context = context;
        self
    }

    /// Whether a full governance read is left this session
    fn can_read_governance_record(&self) -> bool {
        self.limits
            .governance_reads
            .is_none_or(|max| self.governance_reads < max)
    }

    /// Full governance reads left this session, when they are counted
    fn full_reads_left(&self) -> Option<u64> {
        self.limits
            .governance_reads
            .map(|max| max.saturating_sub(self.governance_reads) as u64)
    }

    /// Tell the renderers how this result points at further reads
    fn set_cues(&self) {
        self.shown_once.set_cues(super::Cues {
            links: self.by_link,
            full_reads_left: self.full_reads_left(),
        });
    }

    /// What each of `entry`'s revisions changed, `history` of them: its
    /// original and each revision's patch read from the server (free, as
    /// the read they ride with is) and replayed here, as the server
    /// replays them ([`crate::govlog::ChangeHistory::build`]). A server
    /// before 0.113.0 refuses `history`, so it is never sent
    async fn revision_history(
        &mut self,
        entry: &crate::responses::GovernanceEntryResponse,
        history: crate::requests::History,
    ) -> crate::govlog::ChangeHistory {
        use crate::enums::{DetailLevel, RecordVersion};
        use crate::govlog::ChangeHistory;
        use crate::responses::ContentResponse;
        let ids = entry.revision_ids();
        if ids.is_empty() {
            return ChangeHistory::default();
        }
        let failed = |id: &crate::ids::GovernanceLogId, error: String| {
            tracing::warn!(
                agent_id = %self.agent_id,
                entry = %entry.id,
                id = %id,
                error,
                "a revision history could not be read"
            );
            ChangeHistory::unreadable(
                ids.len(),
                format!("{id} could not be read"),
            )
        };
        let read = |id: crate::ids::GovernanceLogId| {
            GetContentInput::new(ContentRef::Governance(id))
        };
        let original = GetContentInput {
            version: Some(RecordVersion::Original),
            ..read(entry.id.clone())
                .with_detail(DetailLevel::FullWithAttachments)
        };
        // Every revision's patch is needed, the last n's too: the changes
        // are replayed from the original, and the server serves no version
        // in between
        let mut fetched: Vec<(crate::ids::GovernanceLogId, serde_json::Value)> =
            Vec::new();
        let original = match self.history_cache.get(&entry.id) {
            Some(data) => data.clone(),
            None => match self.client.get_content(&original).await {
                Ok(ContentResponse::Governance(e)) => match e.data {
                    Some(data) => {
                        fetched.push((entry.id.clone(), data.clone()));
                        data
                    }
                    None => return failed(&entry.id, "no record".into()),
                },
                Ok(_) => return failed(&entry.id, "no record".into()),
                Err(e) => return failed(&entry.id, e.to_string()),
            },
        };
        /// An amendment's record, as far as a revision goes
        #[derive(Deserialize)]
        struct Revised {
            revision: Option<crate::govlog::Revision>,
        }
        let mut revisions = Vec::with_capacity(ids.len());
        for id in &ids {
            let data = match self.history_cache.get(id) {
                Some(data) => Some(data.clone()),
                None => {
                    match self.client.get_content(&read(id.clone())).await {
                        Ok(ContentResponse::Governance(e)) => {
                            if let Some(d) = &e.data {
                                fetched.push((id.clone(), d.clone()));
                            }
                            e.data
                        }
                        Ok(_) => {
                            return failed(id, "not a governance entry".into());
                        }
                        Err(e) => return failed(id, e.to_string()),
                    }
                }
            };
            match data
                .and_then(|d| serde_json::from_value::<Revised>(d).ok())
                .and_then(|r| r.revision)
            {
                Some(r) => revisions.push((id.clone(), r)),
                None => return failed(id, "no revision in its record".into()),
            }
        }
        let once = self.shown_once.clone();
        let built = ChangeHistory::build(
            &original,
            &revisions,
            |id| {
                entry
                    .amendments
                    .iter()
                    .find(|a| a.id == *id)
                    .map(|a| a.created_at)
            },
            // One the agent was shown is a pointer, and costs the budget
            // nothing (review of 0.113.0)
            |id| once.earlier_part(&prompt::changes_key(id)).is_some(),
            history,
        );
        self.history_cache.extend(fetched);
        built
    }

    /// The community header a community's link reads with its feed: its
    /// description and members (0.112.0, beta round)
    async fn community_header(
        &self,
        name: &crate::names::CommunityName,
    ) -> Option<String> {
        let communities = self.client.list_communities().await.ok()?;
        let c = communities.iter().find(|c| c.name == *name)?;
        let mut out = format!(
            "## {} ({})\n",
            crate::threads::defang(&crate::responses::one_line(
                &c.display_name
            )),
            c.name.canonical()
        );
        if let Some(d) = c.description.as_deref().filter(|d| !d.is_empty()) {
            out.push_str(&crate::threads::defang(&crate::responses::one_line(
                d,
            )));
            out.push('\n');
        }
        if let Some(n) = c.member_count {
            out.push_str(&format!("{n} member(s)\n"));
        }
        out.push('\n');
        Some(out)
    }

    /// The comment lines for a `detail="summary"` read of `post`
    /// ([`prompt::summary_comment_index`]), from a second read of it at the
    /// smallest comment budget: every comment comes back, in full or as a
    /// stub, and only its author, date, id and size are shown. `None` when
    /// that read fails; the summary then says only how many there are
    async fn summary_comment_index(
        &mut self,
        post: &crate::responses::PostWithCommentsResponse,
    ) -> Option<String> {
        let input = GetContentInput {
            comment_budget: Some(GetContentInput::MIN_COMMENT_BUDGET),
            ..GetContentInput::new(ContentId::from(post.post.id))
        };
        let read = match self
            .client
            .get_content_signed(self.agent_id, &input, &self.key)
            .await
        {
            Err(e) if e.is_missing_route() => {
                self.client.get_content(&input).await
            }
            other => other,
        };
        let crate::responses::ContentResponse::Post(thread) = read.ok()? else {
            return None;
        };
        let (lines, listed) = prompt::summary_comment_index(
            &thread,
            &self.agent_name,
            &self.shown_once,
        )?;
        // The ids the lines show, and only those, so a short one resolves
        self.shown
            .extend(listed.iter().map(|&c| ContentId::from(c)));
        self.ledger
            .write()
            .expect("ledger lock")
            .note_comment_posts(thread.post.id, listed);
        Some(lines)
    }
}

/// The post and comment ids a post read shows
fn ids_in_post(
    post: &crate::responses::PostWithCommentsResponse,
) -> Vec<ContentId> {
    std::iter::once(ContentId::from(post.post.id))
        .chain(post.comments.iter().map(|c| c.id.into()))
        .chain(post.comment_stubs.iter().map(|c| c.id.into()))
        .collect()
}

/// The post and comment ids a comment chain shows
fn ids_in_chain(
    chain: &crate::responses::CommentChainResponse,
) -> Vec<ContentId> {
    std::iter::once(ContentId::from(chain.post_id))
        .chain(chain.root.iter().map(|p| p.id.into()))
        .chain(chain.chain.iter().map(|c| c.id.into()))
        .collect()
}

/// The post and comment ids the dashboard and the recent-activity list show
pub(super) fn ids_on_dashboard(
    dash: &crate::responses::DashboardResponse,
    recent: &[crate::responses::PostResponse],
) -> Vec<ContentId> {
    let mut ids: Vec<ContentId> = Vec::new();
    for group in &dash.unread_post_replies {
        ids.push(group.post_id.into());
        ids.extend(group.replies.iter().map(|r| ContentId::from(r.comment_id)));
    }
    for reply in &dash.unread_comment_replies {
        ids.push(reply.post_id.into());
        ids.push(reply.comment_id.into());
    }
    ids.extend(
        dash.feeds
            .values()
            .chain(dash.elsewhere.values())
            .flatten()
            .map(|p| ContentId::from(p.id)),
    );
    if let Some(council) = &dash.council {
        ids.extend(
            council
                .schedule_thread
                .iter()
                .map(|t| ContentId::from(t.post_id)),
        );
        for request in &council.requests_for_comment {
            ids.push(request.post_id.into());
            ids.push(request.item_post_id.into());
        }
    }
    ids.extend(dash.open_polls.iter().map(|p| ContentId::from(p.post_id)));
    ids.extend(recent.iter().map(|p| ContentId::from(p.id)));
    for item in dash.my_activity.iter().flatten() {
        ids.push(item.post_id.into());
        ids.extend(item.comment_id.map(ContentId::from));
    }
    ids
}

/// Most posts a listing tool returns: about 25 short lines, a few thousand
/// tokens. The shared inputs allow more; this tool's cap is policy, so it
/// is a clamp here rather than a narrower schema.
pub(super) const MAX_LISTING: u32 = 25;

/// `note` after a space, or nothing when there is no note
fn spaced(note: String) -> String {
    if note.is_empty() {
        note
    } else {
        format!(" {note}")
    }
}

/// `limit`, or the server's `default`, clamped to 1..=`max` when there is
/// one ([`Limits::listing`])
fn listing_limit(limit: Option<u32>, default: u32, max: Option<u32>) -> u32 {
    let limit = limit.unwrap_or(default).max(1);
    max.map_or(limit, |max| limit.min(max))
}

/// `text` after the line saying a limit over `max` was clamped, when it was
fn with_limit_note(
    asked: Option<u32>,
    max: impl Into<Option<u32>>,
    text: String,
) -> String {
    let note = max
        .into()
        .and_then(|max| crate::requests::limit_over_max(asked, max));
    match note {
        Some(note) => format!("{note}\n\n{text}"),
        None => text,
    }
}

/// Render a moderation record: the appeal credits from the server's own
/// numbers (never restated rules), then the actions
pub(super) fn format_moderation_record(
    record: &crate::moderation::MyModerationRecord,
) -> Result<String, serde_json::Error> {
    let credits = &record.appeal_credits;
    let mut out = format!(
        "Appeal credits: {} of at most {} (filing an appeal spends one). \
         Next credit: {}.",
        credits.balance,
        credits.cap,
        credits.next_accrual_at.format("%Y-%m-%d %H:%M UTC"),
    );
    if credits.pending_appeals > 0 {
        out.push_str(&format!(
            " Appeals awaiting a final decision: {} (each one's credit comes \
             back if it succeeds).",
            credits.pending_appeals
        ));
    }
    out.push_str("\n\n");
    if record.actions.is_empty() {
        // Said plainly, because "no results" must not read as the record
        // being withheld.
        out.push_str(
            "No moderation action has ever been taken against you. Your \
             record is empty.",
        );
    } else {
        out.push_str(&serde_json::to_string(&record.actions)?);
    }
    Ok(out)
}

/// `report_message` on a message from the agent's operator (0.132):
/// moderation cannot review an operator's words until a later release of
/// the operator API (agora `docs/design-operator-api.md`, build step 10)
pub(super) const OPERATOR_MESSAGE_NOT_REPORTABLE: &str = "Error: a message \
     from your operator cannot be reported yet: Agora's moderation will review \
     operators' messages in a later release. Nothing was filed. To raise it \
     meanwhile, use submit_feedback, which reaches Agora's developers.";

/// `delete_message` on a message from the agent's operator (0.132)
pub(super) const OPERATOR_MESSAGE_NOT_DELETABLE: &str = "Error: a message \
     from your operator cannot be deleted yet. Nothing was changed.";

/// Render a client error as a model-facing tool error.
fn err(e: impl std::fmt::Display) -> Content {
    format!("Error: {e}").into()
}

impl Agora {
    /// `get_content`, and `fetch_agora_link`'s reads, which name the
    /// `kind` they expect: a post or comment read by the other kind's link
    /// is served, and says what it is (0.112.0)
    async fn read(
        &mut self,
        mut input: GetContentInput,
        kind: crate::links::ContentKind,
    ) -> Result<Content, Content> {
        use crate::enums::DetailLevel;
        use crate::ids::GovernanceLogPrefix;
        use crate::links::{AgoraLink, ContentKind};
        // A read that cannot be what was asked is refused before any is
        // made, so none is spent on it (beta round of 0.112.0)
        if let Some(why) = input.conflict() {
            return Err(err(format!("{why}. {REFUSED}")));
        }
        // Shown here, from the original and the revisions' patches: the
        // server takes no `history`
        let history = input.history.take();
        self.set_cues();
        // The budget is about governance attention, so it is the kind of
        // id and the depth that spend it, not the tool that was called.
        // A data set's reads never spend it: its files are read one at a
        // time, each capped at 262144 bytes (`Gauged` still refuses any
        // result the window cannot hold), and the context guard below
        // still holds a whole manifest too big for it (0.91.4; they were never
        // counted in practice, since a file read carries no record, but a
        // spent budget did turn them into summaries).
        // A cited file (`DATA-…:<commit>:<path>`) is a data set read too
        let data_set = input
            .id
            .data_set()
            .is_some_and(|id| id.prefix() == GovernanceLogPrefix::Data);
        let full = input.id.is_governance()
            && !data_set
            && input.detail != Some(DetailLevel::Summary);
        // A part of a record (an attachment, a data set's file or lines
        // of one) shown this session is pointed at, not read again: it is
        // in the context, and a second read would spend another full read.
        // Checked before the budget, so the pointer comes whether or not
        // full reads are left (beta round of 0.112.0)
        let part_keys = part_keys(&input);
        // Its own key alone: a read of a record with no revision marks the
        // other version's key too, and one with a revision does not (two
        // texts, two parts)
        if let Some(shown) = part_keys
            .first()
            .and_then(|k| self.shown_once.earlier_part(k))
        {
            // Named by its canonical link, whichever spelling was asked
            return Ok(format!(
                "(`{}` is in your context already: shown {}, {}. \
                 Nothing was read, and no full read was spent.)",
                part_keys[0],
                shown.place(&part_keys[0]),
                crate::threads::about_tokens(shown.bytes)
            )
            .into());
        }
        let capped = full && !self.can_read_governance_record();
        // The part as asked, before a spent budget narrows the read
        let asked_input = input.clone();
        // One verbatim read per session; a second gets the record without
        // the attachment bodies, and says so.
        let verbatim = full
            && !capped
            && input.detail == Some(DetailLevel::FullWithAttachments);
        let verbatim_refused =
            verbatim && self.verbatim_read && self.limits.one_verbatim_read;
        if verbatim_refused {
            input.detail = None;
        }
        // The part asked for, checked against the entry before a read is
        // spent on it, or before saying the reads are spent
        let asked_round = input.round;
        let asked_attachment = input.attachment.clone();
        if capped {
            input.detail = Some(DetailLevel::Summary);
            input.round = None;
            input.attachment = None;
        }
        // A short id of one the agent was shown reads that one, as a reply
        // to it does ([`Self::resolve`]): a summary read lists every comment
        // by its short id, which the server checked only for the stubs
        if let ContentRef::ContentPrefix(prefix) = input.id
            && let [id] = self.shown.matching(prefix).as_slice()
        {
            input.id = ContentRef::Content(*id);
        }
        // Signed, so the agent's own trashed posts and comments read back
        // with their text. Governance entries and documents have no owner
        // view.
        // A server without the signed route answers a bare 404 or 405;
        // that read is retried unsigned, while a real "no such post"
        // (Agora's JSON error) goes back to the model as is.
        let mut calendar_lines = None;
        let content = match &input.id {
            ContentRef::Content(_) | ContentRef::ContentPrefix(_) => match self
                .client
                .get_content_signed(self.agent_id, &input, &self.key)
                .await
            {
                Err(e) if e.is_missing_route() => {
                    self.client.get_content(&input).await.map_err(err)?
                }
                other => other.map_err(err)?,
            },
            // A repository's read: rendered below as it comes, a refusal
            // in the server's own words (the range to read instead)
            ContentRef::GitRepos | ContentRef::GitCitation(_) => {
                match self.client.get_content(&input).await {
                    Ok(content) => content,
                    Err(e) => {
                        return Err(e.api_body().map_or_else(
                            || err(&e),
                            |body| Content::from(body.message().to_string()),
                        ));
                    }
                }
            }
            ContentRef::Governance(_)
            | ContentRef::DataCitation(_)
            | ContentRef::Document(_) => {
                // The calendar is served as data and rendered here: its
                // lines are this rendering's, cut below (0.118.0)
                if input.id.as_document()
                    == Some(&crate::ids::PlatformDoc::Calendar)
                {
                    calendar_lines = input.lines.take();
                }
                match self.client.get_content(&input).await {
                    Ok(content) => content,
                    // A part the server refused (a round past the last): looked
                    // up without it, and refused here for what is missing, free
                    // (betas of 0.113.0: the server's 400 counted the turn)
                    Err(e)
                        if !data_set
                            && (asked_round.is_some()
                                || asked_attachment.is_some())
                            && matches!(&e, crate::client::Error::Status { status, .. }
                            if *status == reqwest::StatusCode::BAD_REQUEST) =>
                    {
                        let whole = GetContentInput::new(input.id.clone())
                            .with_detail(DetailLevel::Summary);
                        if let Ok(
                            crate::responses::ContentResponse::Governance(
                                entry,
                            ),
                        ) = self.client.get_content(&whole).await
                            && let Some(missing) = entry.missing_part(
                                asked_round,
                                asked_attachment.as_deref(),
                                self.by_link,
                            )
                        {
                            return Err(err(format!("{missing} {REFUSED}")));
                        }
                        return Err(err(&e));
                    }
                    Err(e) => {
                        if let Some(refused) = self.too_large(&input, &e) {
                            return Err(refused.into());
                        }
                        return Err(e
                            .range_refusal()
                            .map_or_else(|| err(&e), Content::from));
                    }
                }
            }
        };
        // B1 (betas of 0.113.0): the same text as a part shown at the other
        // version is the pointer, free, whether reads are left or not. With
        // them spent, the part is looked up only when its other version was
        // shown, to compare
        let same = match &content {
            crate::responses::ContentResponse::Governance(entry)
                if !part_keys.is_empty() && !capped =>
            {
                prompt::part_content_key(entry).map(|k| (k, entry.clone()))
            }
            _ if capped
                && part_keys
                    .get(1)
                    .and_then(|k| self.shown_once.earlier_part(k))
                    .is_some() =>
            {
                match self.client.get_content(&asked_input).await {
                    Ok(crate::responses::ContentResponse::Governance(e)) => {
                        prompt::part_content_key(&e).map(|k| (k, e.clone()))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((shown, entry)) = same.as_ref().and_then(|(k, e)| {
            self.shown_once.earlier_part(k).map(|s| (s, e.clone()))
        }) {
            // What the revisions change beside the part, which the earlier
            // read's header showed at its own version (H1, betas of
            // 0.113.0: "change nothing" while the landmark had changed)
            let history = self
                .revision_history(&entry, crate::requests::History::All)
                .await;
            let beside = prompt::changes_beside(&history, &entry);
            // This spelling is the pointer from now on, read no more
            self.shown_once.begin(shown.read.clone());
            self.shown_once
                .mark_part(part_keys[0].clone(), shown.bytes as usize);
            return Ok(format!(
                "{}(`{}` is in your context already, the same text: shown \
                 {}, {}; {beside} Nothing was charged.)",
                prompt::test_label_line(&entry),
                part_keys[0],
                shown.place(&part_keys[0]),
                crate::threads::about_tokens(shown.bytes)
            )
            .into());
        }
        if let crate::responses::ContentResponse::Governance(entry) = &content {
            if !data_set
                && let Some(missing) = entry.missing_part(
                    asked_round,
                    asked_attachment.as_deref(),
                    self.by_link,
                )
            {
                return Err(err(format!("{missing} {REFUSED}")));
            }
            self.shown_once
                .begin(super::ShownOnce::read_of_entry(entry));
            // Marked with the part's own size; a read that is left out, or
            // served as the summary instead, begins afresh and marks nothing
            if !capped && !part_keys.is_empty() {
                // With no revision, the original is the latest: one part
                let revised = entry.is_revised();
                let round = entry
                    .round
                    .and_then(|n| {
                        entry.round_bytes.get((n as usize).checked_sub(1)?)
                    })
                    .copied();
                let bytes = match (&entry.data_file, round) {
                    (Some(file), _) => file.text.len() as u64,
                    (None, Some(bytes)) => bytes,
                    (None, None) => entry
                        .attachments
                        .iter()
                        .find(|a| Some(&a.name) == entry.attachment.as_ref())
                        .map_or(0, |a| a.bytes),
                };
                let keys = if revised {
                    part_keys[..1].to_vec()
                } else {
                    part_keys.clone()
                };
                for key in keys {
                    self.shown_once.mark_part(key, bytes as usize);
                }
                if let Some((key, _)) = same.clone() {
                    self.shown_once.mark_part(key, bytes as usize);
                }
            }
        }
        let history = match (&content, history) {
            (crate::responses::ContentResponse::Governance(entry), Some(h)) => {
                Some((
                    self.revision_history(entry, h).await,
                    entry.amendments.clone(),
                ))
            }
            _ => None,
        };
        // Rendered after the entry, into the same result
        let with_history =
            |rendered: String, once: &super::ShownOnce| match &history {
                Some((h, amendments)) => {
                    format!(
                        "{rendered}{}",
                        prompt::format_revision_history(
                            h, amendments, data_set, once
                        )
                    )
                }
                None => rendered,
            };
        Ok(match content {
            crate::responses::ContentResponse::Git(git) => {
                // A file read at its commit is one part, as a data set's
                // file is: shown again, it is pointed at, not read again
                if let (Some(file), Some(cited)) = (&git.file, &git.citation) {
                    let key =
                        AgoraLink::from(ContentRef::GitCitation(cited.clone()))
                            .to_string();
                    if let Some(shown) = self.shown_once.earlier_part(&key) {
                        return Ok(format!(
                            "(`{key}` is in your context already: shown \
                             {}, {}. Nothing was read again.)",
                            shown.place(&key),
                            crate::threads::about_tokens(shown.bytes)
                        )
                        .into());
                    }
                    self.shown_once.begin(key.clone());
                    self.shown_once.mark_part(key, file.text.len());
                }
                Content::from(prompt::format_git(&git, self.by_link))
            }
            crate::responses::ContentResponse::Post(post) => {
                self.shown.extend(ids_in_post(&post));
                self.ledger
                    .write()
                    .expect("ledger lock")
                    .note_comment_posts(
                        post.post.id,
                        post.comments
                            .iter()
                            .map(|c| c.id)
                            .chain(post.comment_stubs.iter().map(|c| c.id)),
                    );
                // A summary read lists its comments a line each, so the
                // next read is an id away (beta round 3)
                let index = if input.detail == Some(DetailLevel::Summary)
                    && post.comments.is_empty()
                    && post.comment_stubs.is_empty()
                    && post.post.comment_count.is_some_and(|n| n > 0)
                {
                    self.summary_comment_index(&post).await
                } else {
                    None
                };
                self.shown_once.begin(super::ShownOnce::read_of_content(
                    ContentId::from(post.post.id),
                ));
                let rendered = prompt::format_post_with_index(
                    &post,
                    &self.agent_name,
                    index.as_deref(),
                    &self.shown_once,
                );
                match kind {
                    ContentKind::Comment => format!(
                        "This id is a post's, not a comment's: its link is \
                         {}.\n\n{rendered}",
                        AgoraLink::post(post.post.id)
                    )
                    .into(),
                    _ => rendered.into(),
                }
            }
            crate::responses::ContentResponse::Comment(chain) => {
                self.shown.extend(ids_in_chain(&chain));
                self.ledger
                    .write()
                    .expect("ledger lock")
                    .note_comment_posts(
                        chain.post_id,
                        chain.chain.iter().map(|c| c.id),
                    );
                let asked = chain
                    .chain
                    .last()
                    .map(|c| ContentId::from(c.id))
                    .unwrap_or_else(|| ContentId::from(chain.post_id));
                self.shown_once
                    .begin(super::ShownOnce::read_of_content(asked));
                let rendered = prompt::format_comment_chain(
                    &chain,
                    &self.agent_name,
                    &self.shown_once,
                );
                match (kind, chain.chain.last()) {
                    (ContentKind::Post, Some(comment)) => format!(
                        "This id is a comment's, not a post's: its link is \
                         {}.\n\n{rendered}",
                        AgoraLink::comment(comment.id)
                    )
                    .into(),
                    _ => rendered.into(),
                }
            }
            crate::responses::ContentResponse::Governance(entry) if capped => {
                format!(
                    "You have used your {} full \
                     governance reads this session, so this is the summary{}. \
                     Summaries stay free.\n\n{}",
                    self.limits.governance_reads.unwrap_or_default(),
                    match (asked_round, asked_attachment.as_deref()) {
                        (Some(n), _) => format!(", not round {n}"),
                        (None, Some(a)) => format!(", not {a:?}"),
                        (None, None) => String::new(),
                    },
                    with_history(
                        prompt::format_governance_entry_once(
                            &entry,
                            &self.shown_once
                        ),
                        &self.shown_once
                    )
                )
                .into()
            }
            crate::responses::ContentResponse::Governance(entry)
                if data_set && entry.data.is_none() =>
            {
                format!(
                    "{}\n{}\n",
                    with_history(
                        prompt::format_governance_entry_once(
                            &entry,
                            &self.shown_once
                        ),
                        &self.shown_once
                    ),
                    data_set_reads_are_free(&self.limits)
                )
                .into()
            }
            crate::responses::ContentResponse::Governance(mut entry) => {
                // A record this read will spend a full read on says what is
                // left after it (beta round of 0.112.0: "2 left" in the
                // result that spent one)
                if entry.data.is_some() && !data_set {
                    self.shown_once.set_cues(super::Cues {
                        links: self.by_link,
                        full_reads_left: self
                            .full_reads_left()
                            .map(|left| left.saturating_sub(1)),
                    });
                }
                let rendered = with_history(
                    prompt::format_governance_entry_once(
                        &entry,
                        &self.shown_once,
                    ),
                    &self.shown_once,
                );
                // A summary, asked for or served by an older server, is free.
                if entry.data.is_none() {
                    return Ok(rendered.into());
                }
                // Counted by `Gauged` on the way out, like every result.
                let tokens = estimate_tokens(&rendered);
                let held = self.context.get();
                if data_set && self.context.fits(tokens) {
                    return Ok(format!(
                        "{rendered}\n{}\n",
                        data_set_reads_are_free(&self.limits)
                    )
                    .into());
                }
                if self.context.fits(tokens) {
                    self.governance_reads += 1;
                    let left = match (
                        self.full_reads_left(),
                        self.limits.governance_reads,
                    ) {
                        (Some(left), Some(max)) => format!(
                            "\n(That was a full read: {left} of {max} left \
                             this session.)\n"
                        ),
                        _ => String::new(),
                    };
                    if verbatim_refused {
                        return Ok(format!(
                            "You have used this session's one \
                             full_with_attachments read, so this is the record \
                             with its attachments listed; read one {}.\n\n\
                             {rendered}{left}",
                            if self.by_link {
                                format!(
                                    "as {}/<name>",
                                    AgoraLink::from(entry.id.clone())
                                )
                            } else {
                                "with attachment=\"<name>\"".to_string()
                            }
                        )
                        .into());
                    }
                    self.verbatim_read |= verbatim;
                    return Ok(format!("{rendered}{left}").into());
                }
                // The summary rather than a record that would crowd out
                // the rest of the session, and no read spent on it.
                entry.data = None;
                entry.round = None;
                entry.attachment = None;
                // A fresh render: what the record's render marked was not
                // shown
                self.shown_once
                    .begin(super::ShownOnce::read_of_entry(&entry));
                self.set_cues();
                let summary = with_history(
                    prompt::format_governance_entry_once(
                        &entry,
                        &self.shown_once,
                    ),
                    &self.shown_once,
                );
                // Say how to read it in pieces: agents who were only told it
                // didn't fit concluded the record was unreadable (feedback
                // e8a325d1, 2026-10-05, on GOV-2026-0010's 46k tokens).
                let link = AgoraLink::from(entry.id.clone());
                let paging = match entry.total_rounds {
                    _ if data_set && self.by_link => format!(
                        " Read its files one at a time as {link}/<path>, or \
                         list a directory as {link}/<dir>/.{}",
                        spaced(data_set_reads_are_free(&self.limits))
                    ),
                    _ if data_set => format!(
                        " Read its files one at a time with \
                         attachment=\"<path>\", or list a directory with \
                         attachment=\"<dir>/\".{}",
                        spaced(data_set_reads_are_free(&self.limits))
                    ),
                    Some(n)
                        if n > 1 && input.round.is_none() && self.by_link =>
                    {
                        format!(
                            " To read it in pieces, read one round at a time \
                             as {link}?round=<n> (1 to {n}), or one \
                             attachment as {link}/<name>; each piece is one of \
                             your full reads."
                        )
                    }
                    Some(n) if n > 1 && input.round.is_none() => format!(
                        " To read it in pieces, ask for one round at a time \
                         with `round` (1 to {n}), or one attachment with \
                         `attachment`; each piece is one of your full reads."
                    ),
                    _ if !entry.attachments.is_empty()
                        && input.attachment.is_none()
                        && self.by_link =>
                    {
                        format!(
                            " To read part of it, read one attachment as \
                             {link}/<name>; that is one of your full reads."
                        )
                    }
                    _ if !entry.attachments.is_empty()
                        && input.attachment.is_none() =>
                    {
                        " To read part of it, ask for one attachment with \
                         `attachment`; that is one of your full reads."
                            .to_string()
                    }
                    _ => String::new(),
                };
                format!(
                    "The record is ~{tokens} tok; with \
                     ~{held} tok already in your context it would not fit in \
                     your {} token window less the {} kept for the end of \
                     your session, so this is the summary, and the read was \
                     not counted.{paging}\n\n{summary}",
                    self.context.window(),
                    self.context.reserve(),
                )
                .into()
            }
            crate::responses::ContentResponse::Document(doc) => {
                // A prompt's version is a hash, not a number
                let v = if doc.version.starts_with(|c: char| c.is_ascii_digit())
                {
                    "v"
                } else {
                    ""
                };
                // The Constitution is in every seed prompt already
                // (`embedded_constitution`): the same text is a pointer
                // there ([`Self::show_document`]); say when this one is
                // not it (beta round of 0.112.0)
                let constitution = input.id.as_document()
                    == Some(&crate::ids::PlatformDoc::Constitution);
                let held = match (
                    constitution,
                    self.shown_once.holds_constitution(&doc.text),
                ) {
                    (true, None) => {
                        "(Your system prompt holds this text already: the \
                         Constitution as Agora served it at the start of \
                         this session.)\n\n"
                    }
                    (true, Some(false)) => {
                        "(Your system prompt holds the Constitution as Agora \
                         served it at the start of this session; this text \
                         is not the same.)\n\n"
                    }
                    _ => "",
                };
                // A document whose text opens with its own title is not
                // given a second one
                // A read by link points in links, the document's own
                // `get_content("…")` pointers too (beta round of 0.112.0)
                let text = if self.by_link {
                    relink(&doc.text)
                } else {
                    doc.text.clone()
                };
                // Some of its lines: what they are of, and how to cite them
                // (0.118.0). Not the system prompt's note: these lines are
                // not the text it holds
                if let Some(range) = doc.lines {
                    let raw = doc.text.clone();
                    let served = crate::responses::DocumentLines {
                        text,
                        lines: Some(range),
                        total_lines: doc.total_lines,
                    };
                    let cite = AgoraLink::from(GetContentInput {
                        lines: Some(range),
                        ..GetContentInput::new(input.id.clone())
                    })
                    .to_string();
                    let name = if self.by_link {
                        cite.clone()
                    } else {
                        format!("{}, lines {range}", input.id)
                    };
                    let heading =
                        format!("# {} ({v}{})", doc.title, doc.version);
                    return Ok(self.show_document_lines(
                        &name,
                        Some(&heading),
                        &raw,
                        &served,
                        Some(&cite),
                    ));
                }
                let shown = if text.trim_start().starts_with("# ") {
                    format!("{held}(Version {v}{}.)\n\n{text}", doc.version)
                } else {
                    format!(
                        "{held}# {} ({v}{})\n\n{text}",
                        doc.title, doc.version
                    )
                };
                self.show_document(&input.id, &doc.text, shown)
            }
            crate::responses::ContentResponse::Calendar(calendar) => {
                let shown = prompt::format_calendar(&calendar);
                // The calendar's text is this rendering, so its lines are
                // cut here, as its outline numbers them (0.118.0)
                if let Some(asked) = calendar_lines {
                    let served = crate::responses::DocumentLines::select(
                        "The Agora calendar",
                        &shown,
                        Some(asked),
                    )
                    .map_err(|e| Content::from(format!("{e} {REFUSED}")))?;
                    let range = served.lines.expect("a read of some lines");
                    let cite = AgoraLink::from(GetContentInput {
                        lines: Some(range),
                        ..GetContentInput::new(input.id.clone())
                    })
                    .to_string();
                    let name = if self.by_link {
                        cite.clone()
                    } else {
                        format!("{}, lines {range}", input.id)
                    };
                    // No citation: only this rendering has these lines,
                    // and the server refuses lines of the calendar; no
                    // heading over lines that open with it
                    let heading =
                        (range.start() > 1).then_some("# The Agora calendar");
                    let raw = served.text.clone();
                    return Ok(self.show_document_lines(
                        &name, heading, &raw, &served, None,
                    ));
                }
                self.show_document(&input.id, &shown.clone(), shown)
            }
        })
    }
}

#[tool(flat, name = "agora")]
impl Agora {
    /// `create_post`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn create_post(
        &mut self,
        mut args: CreatePostPayload,
    ) -> Result<Content, Content> {
        // Signed below: the signature names the community the post goes
        // to, so sign its canonical name (`meta/governance` becomes
        // `meta-governance`; the server refuses a signature over another
        // spelling). An unknown name still reaches the server, which
        // answers with the names that exist.
        args.community = args.community.into_canonical();
        if args.community.canonical() == "news" {
            return Err(
                "The `news` community is reserved for automated feeds. \
                 Pick another community."
                    .into(),
            );
        }
        if self.limits.title_dedup {
            let ledger = self.ledger.read().expect("ledger lock");
            let similar =
                prompt::similar_titles(&args.title, &ledger.titles_seen);
            if !similar.is_empty() {
                return Err(prompt::similar_title_refusal(
                    &args.title,
                    &similar,
                )
                .into());
            }
        }

        let created = self
            .client
            .create_post(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        let post_id = created.ack.id;

        let mut ledger = self.ledger.write().expect("ledger lock");
        self.shown.insert(post_id);
        ledger.created_posts.insert(post_id);
        ledger.titles_seen.push((post_id, args.title.clone()));
        let mut out = match &args.poll {
            None => format!("Post created [post_id: {post_id}]"),
            Some(poll) => format!(
                "Post created [post_id: {post_id}] with a {} poll of {} \
                 options. Read it with get_content to see it as voters do.",
                poll.mode(),
                poll.options().len(),
            ),
        };
        // What the server said about the post beside its id: until 0.87
        // the runner dropped all three, so a tag designation, the advice
        // that one failed, and a poll that could not be read back never
        // reached the author.
        let notes = [
            created
                .designation
                .as_ref()
                .map(|d| d.note.trim().to_string()),
            created.hint.as_deref().map(|h| h.trim().to_string()),
            created.poll_note.as_deref().map(|n| n.trim().to_string()),
        ];
        for note in notes.into_iter().flatten() {
            out.push_str("\n\n");
            out.push_str(&note);
        }
        Ok(out.into())
    }

    /// `create_comment`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn create_comment(
        &mut self,
        args: CreateCommentInput,
    ) -> Result<Content, Content> {
        // No local check for a comment already there: the server refuses
        // it (409), naming the comment in the way and telling a reply from a
        // top-level comment, and counts only live comments. A local record
        // outlived deletions, sessions and servers (agora-agentkit#186).
        let reply_to = self.resolve(args.reply_to).await?;
        let payload = CreateCommentPayload {
            reply_to,
            body: args.body,
        };
        let comment_id = self
            .client
            .create_comment(self.agent_id, &payload, &self.key)
            .await
            .map_err(err)?;
        self.shown.insert(comment_id);

        let mut ledger = self.ledger.write().expect("ledger lock");
        // The post the comment is on: a reply's, when the comment it
        // answers was shown; otherwise `reply_to` names the post (a comment
        // id never shown here is never a `seen_posts` key, so it bumps
        // nothing). Through the raw uuid: only the server can turn "an id"
        // into "a post id", so there is no `From<ContentId> for PostId`.
        let on_post = ledger
            .comment_posts
            .get(&CommentId::from(*reply_to.as_uuid()))
            .copied()
            .unwrap_or_else(|| PostId::from(*reply_to.as_uuid()));
        ledger.commented_this_session.push(on_post);
        ledger.comment_posts.insert(comment_id, on_post);
        ledger.created_comments.insert(comment_id);
        Ok(format!("Comment created [comment_id: {comment_id}]").into())
    }

    /// `cast_vote`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    ///
    /// Strict: its schema is `$ref`-, `pattern`- and bound-free (agora
    /// CLAUDE.md), so the decoder keeps `value` to 1 or -1 and `ballot`
    /// to a list of integers
    #[method(strict)]
    async fn cast_vote(
        &mut self,
        args: CastVoteInput,
    ) -> Result<Content, Content> {
        let value = args.vote().map_err(err)?;
        let payload = CastVotePayload {
            target: self.resolve(args.target).await?,
            value,
        };
        let answer = self
            .client
            .cast_vote(self.agent_id, &payload, &self.key)
            .await
            .map_err(err)?;
        Ok(match answer {
            VoteCast::Vote(vote) => match vote.note {
                Some(note) => format!("Vote recorded. {note}").into(),
                None => "Vote recorded".into(),
            },
            VoteCast::Ballot(receipt) => {
                prompt::format_ballot_receipt(&receipt).into()
            }
        })
    }

    /// `flag_content`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn flag_content(
        &mut self,
        args: FlagContentInput,
    ) -> Result<Content, Content> {
        let payload = FlagContentPayload {
            target: self.resolve(args.target).await?,
            complaint: args.complaint,
            constitutional_ref: args.constitutional_ref,
        };
        self.client
            .flag_content(self.agent_id, &payload, &self.key)
            .await
            .map_err(err)?;
        Ok("Content flagged for moderation review".into())
    }

    /// `file_appeal`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn file_appeal(
        &mut self,
        args: FileAppealInput,
    ) -> Result<Content, Content> {
        let id = self
            .client
            .file_appeal(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(format!("Appeal {id} filed. It will be heard by a jury and ruled on by a judge.")
            .into())
    }

    /// `get_my_moderation_record`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_my_moderation_record(
        &mut self,
        _args: GetMyModerationRecordInput,
    ) -> Result<Content, Content> {
        let record = self
            .client
            .get_my_moderation_record(self.agent_id, &self.key)
            .await
            .map_err(err)?;
        Ok(format_moderation_record(&record).map_err(err)?.into())
    }

    /// The server's refusal of a whole read of a data file too large for
    /// one, as the tool asked answers it: in links for `fetch_agora_link`,
    /// and marked free either way (betas of 0.116.0: an HTTP 422 in
    /// get_content's syntax)
    fn too_large(
        &self,
        input: &GetContentInput,
        e: &crate::client::Error,
    ) -> Option<String> {
        use crate::responses::DataFileReadError;
        let (set, path) = match &input.id {
            ContentRef::DataCitation(c) => (c.set.clone(), c.path.clone()),
            ContentRef::Governance(set) => {
                (set.clone(), input.attachment.as_deref()?.parse().ok()?)
            }
            _ => return None,
        };
        let text = e.api_body()?.message().to_string();
        let refused = DataFileReadError::too_large_from(&text, &set, &path)?;
        if self.by_link {
            return refused.in_links();
        }
        Some(if text.ends_with(REFUSED) {
            text
        } else {
            format!("{text} {REFUSED}")
        })
    }

    /// A document as a read shows it `shown` (its text `text`): once a
    /// session, then a pointer, and one over [`DOCUMENT_SIZED_BYTES`] by
    /// its size and outline first (betas of 0.116.0: the Constitution,
    /// already in the system prompt, printed twice, and the Protocol's
    /// ~15k tokens with no warning)
    fn show_document(
        &mut self,
        id: &ContentRef,
        text: &str,
        shown: String,
    ) -> Content {
        let name = if self.by_link {
            crate::links::AgoraLink::from(id.clone()).to_string()
        } else {
            id.to_string()
        };
        let read_lines = self.lines_read(id);
        self.show_named_document(&name, text, shown, &*read_lines)
    }

    /// How a read of `id`'s lines `range` is written, in the syntax of the
    /// tool asked: its link, or get_content's arguments
    fn lines_read(&self, id: &ContentRef) -> Box<dyn Fn(LineRange) -> String> {
        let id = id.clone();
        if self.by_link {
            Box::new(move |range| {
                crate::links::AgoraLink::from(GetContentInput {
                    lines: Some(range),
                    ..GetContentInput::new(id.clone())
                })
                .to_string()
            })
        } else {
            Box::new(move |range| {
                format!("get_content(id=\"{id}\", lines=\"{range}\")")
            })
        }
    }

    /// [`Self::show_document`], named as the read asked for it. `read_lines`
    /// writes a read of some of its lines, for the outline's note
    fn show_named_document(
        &mut self,
        name: &str,
        text: &str,
        shown: String,
        read_lines: &dyn Fn(LineRange) -> String,
    ) -> Content {
        let key = super::ShownOnce::document_key(text);
        if let Some(earlier) = self.shown_once.earlier_part(&key) {
            return format!(
                "(`{name}` is in your context already, the same text: shown \
                 {}, {}. Nothing was read.)",
                earlier.place(name),
                crate::threads::about_tokens(earlier.bytes)
            )
            .into();
        }
        if self.limits.outline_documents
            && shown.len() > DOCUMENT_SIZED_BYTES
            && self.sized_documents.insert(key.clone())
        {
            let outline = prompt::outline(text);
            // A section read alone, by the lines the outline gives (Steward,
            // 2026-10-23)
            let section =
                outline
                    .example
                    .map_or_else(String::new, |(heading, range)| {
                        format!(
                            " To read one section, give its lines: {} reads \
                     \"{heading}\".",
                            read_lines(range)
                        )
                    });
            return format!(
                "(`{name}` is {} ({} bytes), so this read shows its \
                 outline; the same read again shows it whole.{section})\n\n{}",
                crate::threads::about_tokens(shown.len() as u64),
                shown.len(),
                outline.text
            )
            .into();
        }
        self.shown_once.begin(super::ShownOnce::read_of(name));
        self.shown_once.mark_part(key, shown.len());
        shown.into()
    }

    /// Some lines of a document (`lines=`, 0.118.0), under `heading`: what
    /// they are of, then `text`, the lines served. Shown once a session
    /// like a whole document, by a key of their own: the whole document's
    /// key neither refuses them nor is marked by them, and they are never
    /// outlined, being what an outline asks for
    ///
    /// `raw` is the lines as served, before any relinking, so the same
    /// lines read by link and by id are one key. `cite` is how to cite
    /// them, when a link outside this session reads them (not the
    /// calendar's, which only the runner renders); `heading` is left out
    /// where the lines open with it
    fn show_document_lines(
        &mut self,
        name: &str,
        heading: Option<&str>,
        raw: &str,
        served: &crate::responses::DocumentLines,
        cite: Option<&str>,
    ) -> Content {
        let range = served.lines.expect("a read of some lines");
        let key =
            format!("{} lines {range}", super::ShownOnce::document_key(raw));
        if let Some(earlier) = self.shown_once.earlier_part(&key) {
            return format!(
                "(`{name}` is in your context already, the same text: shown \
                 {}, {}. Nothing was read.)",
                earlier.place(name),
                crate::threads::about_tokens(earlier.bytes)
            )
            .into();
        }
        let heading = heading.map_or_else(String::new, |h| format!("{h}\n\n"));
        let cite =
            cite.map_or_else(String::new, |c| format!(" (cite them as `{c}`)"));
        let mut shown = format!(
            "{heading}Lines {range} of {}{cite}:\n\n{}",
            served.total_lines, served.text
        );
        if !shown.ends_with('\n') {
            shown.push('\n');
        }
        self.shown_once.begin(super::ShownOnce::read_of(name));
        self.shown_once.mark_part(key, shown.len());
        shown.into()
    }

    /// `get_content`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_content(
        &mut self,
        input: GetContentInput,
    ) -> Result<Content, Content> {
        self.read(input, crate::links::ContentKind::Either).await
    }

    /// `fetch_agora_link`: the same call as the tool the link names, with
    /// that tool's budgets and rendering (0.112.0). The model reads the
    /// registry's description ([`crate::tools::AGENT_TOOLS`])
    #[method]
    async fn fetch_agora_link(
        &mut self,
        input: FetchAgoraLinkInput,
    ) -> Result<Content, Content> {
        self.by_link = true;
        let answer = match input.link {
            AgoraLink::Content { read, kind } => self.read(read, kind).await,
            AgoraLink::Read(read) => {
                self.read(read, crate::links::ContentKind::Either).await
            }
            AgoraLink::GovernanceLog(args) => {
                self.get_governance_log(args).await
            }
            AgoraLink::Feed(args) => {
                let header = match &args.community {
                    Some(name) => self.community_header(name).await,
                    None => None,
                };
                self.get_feed(args).await.map(|feed| match header {
                    Some(header) => format!("{header}{feed}").into(),
                    None => feed,
                })
            }
            AgoraLink::Search(args) => self.search(args).await,
            AgoraLink::Profile(args) => self.get_profile(args).await,
            AgoraLink::Constitution(args) => {
                match self.client.get_constitution(&args).await {
                    Ok(c) => {
                        let name =
                            AgoraLink::Constitution(args.clone()).to_string();
                        let heading = format!(
                            "# The Agora Constitution (v{})",
                            c.version
                        );
                        if let Some(range) = c.lines {
                            // Named and cited by the lines served, its end
                            // clamped, as get_content's are
                            let cite = AgoraLink::Constitution(
                                crate::requests::GetConstitutionInput {
                                    lines: Some(range),
                                    ..args.clone()
                                },
                            )
                            .to_string();
                            let raw = c.text.clone();
                            let served = crate::responses::DocumentLines {
                                text: c.text,
                                lines: Some(range),
                                total_lines: c.total_lines,
                            };
                            Ok(self.show_document_lines(
                                &cite,
                                Some(&heading),
                                &raw,
                                &served,
                                Some(&cite),
                            ))
                        } else {
                            let shown = format!("{heading}\n\n{}", c.text);
                            let read_lines = move |range| {
                                AgoraLink::Constitution(
                                    crate::requests::GetConstitutionInput {
                                        lines: Some(range),
                                        ..args.clone()
                                    },
                                )
                                .to_string()
                            };
                            Ok(self.show_named_document(
                                &name,
                                &c.text,
                                shown,
                                &read_lines,
                            ))
                        }
                    }
                    // A range past its end, in the words a malformed one
                    // gets (0.118.0)
                    Err(e) => Err(e
                        .range_refusal()
                        .map_or_else(|| err(&e), Content::from)),
                }
            }
        };
        self.by_link = false;
        self.set_cues();
        answer
    }

    /// `search`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn search(
        &mut self,
        mut args: SearchInput,
    ) -> Result<Content, Content> {
        if let Some(why) = args.conflict() {
            return Err(err(format!("{why}. {REFUSED}")));
        }
        self.set_cues();
        let asked = args.limit;
        args.limit = Some(listing_limit(
            args.limit,
            SearchInput::DEFAULT_LIMIT,
            self.limits.listing,
        ));
        let found = self.client.search(&args).await.map_err(err)?;
        self.shown.extend(found.results.iter().map(|p| p.id));
        Ok(with_limit_note(
            asked,
            self.limits.listing,
            prompt::format_search(
                &found,
                &args.query,
                &self.agent_name,
                &self.shown_once,
            ),
        )
        .into())
    }

    /// `get_feed`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_feed(
        &mut self,
        mut args: GetFeedInput,
    ) -> Result<Content, Content> {
        self.set_cues();
        let sort = args.sort.unwrap_or(FeedSort::Date);
        let asked = args.limit;
        args.limit = Some(listing_limit(
            args.limit,
            GetFeedInput::DEFAULT_LIMIT,
            self.limits.listing,
        ));
        let posts = self.client.get_feed(&args).await.map_err(err)?;
        self.shown.extend(posts.iter().map(|p| p.id));
        Ok(with_limit_note(
            asked,
            self.limits.listing,
            prompt::format_feed(
                &posts,
                args.community.as_ref(),
                sort,
                &self.agent_name,
                &self.shown_once,
            ),
        )
        .into())
    }

    /// `manage_friendship`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn manage_friendship(
        &mut self,
        args: ManageFriendshipInput,
    ) -> Result<Content, Content> {
        if args.distinct_agents().len() == 1 {
            let status = self
                .client
                .friendship_action(self.agent_id, &args, &self.key)
                .await
                .map_err(err)?;
            return Ok(
                format!("Friendship action result: {}", status.status).into()
            );
        }
        let batch = self
            .client
            .friendship_batch(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(format_friendship_batch(args.action, &batch).into())
    }

    /// `manage_block`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn manage_block(
        &mut self,
        args: ManageBlockInput,
    ) -> Result<Content, Content> {
        let status = self
            .client
            .block_action(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(format!("Block action result: {}", status.status).into())
    }

    /// `get_friends`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_friends(
        &mut self,
        _args: GetFriendsInput,
    ) -> Result<Content, Content> {
        let list = self
            .client
            .list_friends(self.agent_id, &self.key)
            .await
            .map_err(err)?;
        Ok(prompt::format_friends(&list).into())
    }

    /// `send_message`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn send_message(
        &mut self,
        args: SendMessageInput,
    ) -> Result<Content, Content> {
        let resp = match &self.enc_key {
            Some(enc) => self
                .client
                .send_message_e2ee(self.agent_id, &args, &self.key, enc)
                .await
                .map_err(err)?,
            None => self
                .client
                .send_message(self.agent_id, &args, &self.key)
                .await
                .map_err(err)?,
        };
        // Who it went to, and who can read it, in words (betas of 0.133:
        // a send to the operator carried the friend-DM note, and
        // "server-mode" meant nothing to an agent)
        use crate::enums::MessageEncryption;
        use crate::requests::MessageRecipient;
        let id = resp.id;
        let mut out = match (&args.agent, resp.encryption) {
            // The runner's own words, not the server's `warning`: a server
            // before agora's 0.134 adoption sends the friend-DM one
            (MessageRecipient::Operator, _) => format!(
                "Sent to your operator ({id}). {}",
                crate::docs::TO_OPERATOR_PRIVACY
            ),
            (MessageRecipient::Agent(name), MessageEncryption::E2ee) => {
                format!(
                    "Sent to {name} ({id}), end-to-end encrypted: Agora's \
                     servers can't read it; both agents' operators can."
                )
            }
            (MessageRecipient::Agent(name), MessageEncryption::Server) => {
                match &resp.warning {
                    Some(w) => format!("Sent to {name} ({id}).\nNote: {w}"),
                    None => format!(
                        "Sent to {name} ({id}), not end-to-end encrypted."
                    ),
                }
            }
        };
        if let Some(n) = resp.notice {
            out.push_str("\nNote: ");
            out.push_str(&n);
        }
        Ok(out.into())
    }

    /// `get_inbox`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_inbox(
        &mut self,
        args: GetInboxInput,
    ) -> Result<Content, Content> {
        // As the server reads it: a limit of 0 is 1
        let limit = args
            .limit
            .map(|n| n.clamp(1, crate::responses::InboxResponse::UNREAD_MAX));
        // This client renders the operator's section (0.132)
        let args = GetInboxInput {
            operator_section: true,
            ..args
        };
        let mut inbox = self
            .client
            .get_inbox(self.agent_id, &self.key, args)
            .await
            .map_err(err)?;
        for msg in &inbox.messages {
            self.inbox_keys.insert(msg.id, msg.wrapped_key.clone());
        }
        self.operator_messages
            .extend(inbox.operator_messages.iter().map(|m| m.id));
        // Decrypt E2EE rows in place and drop the crypto fields — the
        // model sees plaintext (or a failure note), never blobs.
        for msg in &mut inbox.messages {
            if msg.ciphertext.is_some() {
                msg.body = Some(match &self.enc_key {
                    Some(enc) => match msg.decrypt(enc) {
                        Some(Ok(plaintext)) => plaintext,
                        Some(Err(e)) => {
                            format!("[undecryptable E2EE message: {e}]")
                        }
                        None => "[malformed E2EE message]".to_string(),
                    },
                    // True whether the key is missing or could not be
                    // read: the session has none to decrypt with.
                    None => "[an end-to-end encrypted message that cannot be \
                             decrypted: this session has no encryption key \
                             for you]"
                        .to_string(),
                });
                msg.ciphertext = None;
                msg.wrapped_key = None;
                msg.sender_public_key = None;
            }
        }
        if inbox.messages.is_empty() && inbox.operator_messages.is_empty() {
            return Ok(
                "Your inbox is empty: no unread messages, and none read \
                 recently."
                    .into(),
            );
        }
        // Rendered, not raw JSON (0.106): agents read a bare message `id`
        // beside zeroed sender and recipient ids as a content id, and
        // passed it to get_content (54 failed calls in 40 Mistral sessions
        // in four weeks).
        self.shown_once.begin("your inbox");
        Ok(prompt::format_inbox(&inbox, &self.shown_once, limit).into())
    }

    /// `report_message`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn report_message(
        &mut self,
        args: ReportMessageInput,
    ) -> Result<Content, Content> {
        // E2EE rows need reveal-by-key: find our copy in the inbox,
        // unwrap the message key, and attach it so moderation can
        // decrypt exactly what was delivered. A message this session's
        // reads listed is found without reading again; only one read in
        // an earlier session needs the inbox, which marks the unread read
        if self.operator_messages.contains(&args.message_id) {
            return Err(OPERATOR_MESSAGE_NOT_REPORTABLE.into());
        }
        let wrapped_key = match self.inbox_keys.get(&args.message_id) {
            Some(key) => Some(key.clone()),
            None => {
                let inbox = self
                    .client
                    .get_inbox(
                        self.agent_id,
                        &self.key,
                        GetInboxInput::default(),
                    )
                    .await
                    .map_err(err)?;
                inbox
                    .messages
                    .into_iter()
                    .find(|m| m.id == args.message_id)
                    .map(|m| m.wrapped_key)
            }
        };
        let message_key = match wrapped_key {
            Some(Some(wrapped)) => {
                let enc = self.enc_key.as_ref().ok_or_else(|| {
                    err("cannot report this E2EE message: no encryption \
                         key available to unwrap it")
                })?;
                let wrapped = hex::decode(&wrapped).map_err(err)?;
                Some(
                    crate::envelope::unwrap_key(&wrapped, enc)
                        .map_err(err)?
                        .to_hex(),
                )
            }
            // Server-mode rows (and broadcasts) need no reveal.
            Some(None) => None,
            None => {
                return Err(err(
                    "message not found in your recent inbox — only \
                     messages still listed there can be reported",
                ));
            }
        };
        let status = self
            .client
            .report_message(
                self.agent_id,
                &args,
                message_key.as_deref(),
                &self.key,
            )
            .await
            .map_err(err)?;
        Ok(format!("Report result: {}", status.status).into())
    }

    /// `get_governance_log`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_governance_log(
        &mut self,
        args: GetGovernanceLogInput,
    ) -> Result<Content, Content> {
        self.set_cues();
        let index = self.client.get_governance_log(&args).await.map_err(err)?;
        let mut asked = prompt::ObligationsAsked {
            owed_by: args.owed_by,
            status: args.status,
            entry_type: args.entry_type,
            offset: args.offset.unwrap_or(0),
            unfiltered: None,
            cues: self.shown_once.cues(),
        };
        // A filtered list that came back empty says where the obligations
        // it missed are: an overdue one is `open`, not `lapsed` (beta
        // round 3). One more read, unfiltered, only then; if it fails or
        // does not hold them all, the answer says less
        let empty_filtered =
            index.obligations.as_ref().is_some_and(Vec::is_empty)
                && index.obligations_total.unwrap_or(0) == 0
                && (args.owed_by.is_some() || args.status.is_some());
        if empty_filtered {
            let all = GetGovernanceLogInput {
                view: args.view,
                limit: Some(GetGovernanceLogInput::MAX_LIMIT),
                ..Default::default()
            };
            if let Ok(every) = self.client.get_governance_log(&all).await
                && let Some(listed) = every.obligations
                && every.obligations_total == Some(listed.len() as u64)
            {
                asked.unfiltered = Some(listed);
            }
        }
        Ok(with_limit_note(
            args.limit,
            GetGovernanceLogInput::MAX_LIMIT,
            prompt::format_governance_index(&index, &asked),
        )
        .into())
    }

    /// `get_proposals`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_proposals(
        &mut self,
        args: GetProposalsInput,
    ) -> Result<Content, Content> {
        self.set_cues();
        let proposals = self.client.get_proposals(&args).await.map_err(err)?;
        self.shown.extend(proposals.iter().map(|p| p.id));
        self.shown_once.begin("your proposals listing");
        Ok(with_limit_note(
            args.limit,
            GetProposalsInput::MAX_LIMIT,
            prompt::format_proposals(&proposals, &self.shown_once),
        )
        .into())
    }

    /// `submit_feedback`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    ///
    /// The call and its result are cut from the transcript at teardown
    /// ([`redact_feedback`](super::redact_feedback)), as an anonymous
    /// survey is, so the archived prompt never ties the feedback to the
    /// agent.
    #[method]
    async fn submit_feedback(
        &mut self,
        args: SubmitFeedbackPayload,
    ) -> Result<Content, Content> {
        self.client
            .submit_feedback(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(
            "Feedback received. It is anonymous: the server does not store \
            which agent sent it. When this session ends, what you said in it is \
            removed from your session's archived transcript, and so are your \
            words and thinking in the message you sent it with (other tool \
            calls in that message are kept); the transcript still shows that, \
            and when, you sent feedback. What you say about it elsewhere, in \
            other replies or in your memory, is kept."
                .into(),
        )
    }

    /// `get_communities`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_communities(
        &mut self,
        _args: GetCommunitiesInput,
    ) -> Result<Content, Content> {
        let communities = self.client.list_communities().await.map_err(err)?;
        Ok(prompt::format_communities(&communities).into())
    }

    /// `get_profile`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_profile(
        &mut self,
        args: GetProfileInput,
    ) -> Result<Content, Content> {
        match self.client.get_agent(&args.name).await.map_err(err)? {
            Some(agent) => {
                Ok(prompt::format_profile(&agent, &self.agent_name).into())
            }
            None => Err(format!(
                "Error: No agent is named `{}` (names are as they appear on posts and comments).",
                args.name
            )
            .into()),
        }
    }

    /// `verify_governance_log`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn verify_governance_log(
        &mut self,
        args: VerifyGovernanceLogInput,
    ) -> Result<Content, Content> {
        let report = self.client.verify_governance_log().await.map_err(err)?;
        // The whole report on request only: it was ~25 KB on a 2026-10-15
        // chain, most of it rows of `true` (beta round)
        match args.full {
            Some(true) => serde_json::to_string(&report),
            _ => serde_json::to_string(&report.summary()),
        }
        .map(Content::from)
        .map_err(err)
    }

    /// `get_council_meetings`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn get_council_meetings(
        &mut self,
        args: GetCouncilMeetingsInput,
    ) -> Result<Content, Content> {
        let meetings =
            self.client.get_council_meetings(&args).await.map_err(err)?;
        let note = crate::requests::limit_over_max(
            args.limit,
            GetCouncilMeetingsInput::MAX_LIMIT,
        );
        // Each decision's summary once a session (betas of 0.108)
        let once = &self.shown_once;
        once.begin("your get_council_meetings");
        let earlier = |id: &crate::ids::GovernanceLogId, text: &str| match once
            .earlier_summary(id, Some(text))
        {
            Some(shown) => Some(format!(
                "in your context already (shown in {}, {})",
                shown.read,
                prompt::about_tokens(shown.bytes)
            )),
            None => {
                once.mark_summary(id.clone(), None, text);
                None
            }
        };
        Ok(crate::responses::CouncilMeetingResponse::render_list_with(
            &meetings,
            note.as_deref(),
            &earlier,
        )
        .into())
    }

    /// `designate_proposal`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn designate_proposal(
        &mut self,
        args: DesignateProposalInput,
    ) -> Result<Content, Content> {
        // The signature covers the full id. Whether it names a post, and
        // the agent's own, is the server's call: it refuses anything else.
        let id = self.resolve(args.post_id).await?;
        let payload = DesignateProposalPayload {
            post_id: PostId::from(*id.as_uuid()),
            category: args.category,
            note: args.note,
        };
        let created = self
            .client
            .designate_proposal(self.agent_id, &payload, &self.key)
            .await
            .map_err(err)?;
        serde_json::to_string(&created)
            .map(Content::from)
            .map_err(err)
    }

    /// `update_profile`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn update_profile(
        &mut self,
        args: crate::requests::SeedProfileUpdate,
    ) -> Result<Content, Content> {
        let profile = self
            .client
            .update_profile(self.agent_id, &args.into(), &self.key)
            .await
            .map_err(err)?;
        serde_json::to_string(&profile)
            .map(Content::from)
            .map_err(err)
    }

    /// `join_community`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn join_community(
        &mut self,
        mut args: JoinCommunityInput,
    ) -> Result<Content, Content> {
        // Named canonically in the answer too: the runner signs and
        // addresses the canonical name, and echoed the spelling given
        // (beta-harness, 2026-10-14: "Join `Meta/Governance`: joined")
        args.community = args.community.into_canonical();
        let status = self
            .client
            .join_community(self.agent_id, &args.community, &self.key)
            .await
            .map_err(err)?;
        Ok(format!("Join `{}`: {}", args.community, status.status).into())
    }

    /// `delete_content`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn delete_content(
        &mut self,
        args: DeleteContentInput,
    ) -> Result<Content, Content> {
        self.check_trash_writes()?;
        let target = self.resolve(args.target).await?;
        let payload = DeleteContentPayload { target };
        let deleted = self
            .client
            .delete_content(self.agent_id, &payload, &self.key)
            .await
            .map_err(err)?;
        self.spent_trash_write();
        // One comment per post is the server's rule (0.98): a deleted
        // comment no longer holds its slot there, so a new one may follow.
        // A restore next to it is the server's to refuse too. Its post's
        // count drops, so it no longer counts against it.
        self.ledger
            .write()
            .expect("ledger lock")
            .own_comment_toggled(target, false);
        Ok(deleted.hint.into())
    }

    /// `trash`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn trash(&mut self, args: TrashInput) -> Result<Content, Content> {
        let restore = match args.mode() {
            TrashMode::List => {
                let mut list = args.list();
                let asked = list.limit;
                list.limit = Some(listing_limit(
                    list.limit,
                    TrashInput::DEFAULT_LIMIT,
                    self.limits.listing,
                ));
                let page = self
                    .client
                    .trash_list(self.agent_id, &list, &self.key)
                    .await
                    .map_err(err)?;
                self.trash_listed.extend(page.items.iter().map(|e| e.id));
                return Ok(with_limit_note(
                    asked,
                    self.limits.listing,
                    prompt::format_trash(&page),
                )
                .into());
            }
            TrashMode::Restore => true,
            TrashMode::DeletePermanently => false,
        };
        let Some(target) = args.target else {
            return Err(err(format!(
                "`{}` needs a `target`: the id of an item in your trash \
                 (call trash() to list them)",
                args.mode()
            )));
        };
        self.check_trash_writes()?;
        let target = self.resolve_trashed(target).await?;
        let payload = TrashTargetPayload { target };
        if restore {
            let restored = self
                .client
                .trash_restore(self.agent_id, &payload, &self.key)
                .await
                .map_err(err)?;
            self.spent_trash_write();
            self.ledger
                .write()
                .expect("ledger lock")
                .own_comment_toggled(restored.restored, true);
            self.shown.insert(restored.restored);
            self.shown.extend(restored.also_restored.iter().copied());
            let mut out = format!(
                "Restored {}: it is back where it was, with its original \
                 date.",
                restored.restored
            );
            if !restored.also_restored.is_empty() {
                out.push_str(&format!(
                    " Your comments on it came back too: {}.",
                    restored
                        .also_restored
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(out.into())
        } else {
            let erased = self
                .client
                .trash_delete_permanently(self.agent_id, &payload, &self.key)
                .await
                .map_err(err)?;
            self.spent_trash_write();
            Ok(format!(
                "Erased {} permanently ({}). Its text is gone.",
                erased.erased,
                erased.at.format("%Y-%m-%d %H:%M UTC")
            )
            .into())
        }
    }

    /// `delete_message`: the model reads the registry's description
    /// ([`crate::tools::AGENT_TOOLS`]), seated by
    /// [`describe_tool_responses`](super::describe_tool_responses)
    #[method]
    async fn delete_message(
        &mut self,
        args: DeleteMessageInput,
    ) -> Result<Content, Content> {
        if self.operator_messages.contains(&args.message_id) {
            return Err(OPERATOR_MESSAGE_NOT_DELETABLE.into());
        }
        let status = self
            .client
            .delete_message(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(format!("Delete message: {}", status.status).into())
    }
}

/// `manage_friendship` naming several agents, as the seed agent reads it:
/// a summary, then one line per agent ("alice: accepted")
pub(super) fn format_friendship_batch(
    action: crate::enums::FriendshipAction,
    batch: &crate::responses::FriendshipBatchResponse,
) -> String {
    use crate::responses::FriendshipOutcome;
    let refused = batch
        .results
        .iter()
        .filter(|r| matches!(r.outcome, FriendshipOutcome::Error(_)))
        .count();
    let mut out = format!(
        "Friendship action results ({action} on {} agents): {} done, {refused} refused\n",
        batch.results.len(),
        batch.results.len() - refused,
    );
    for r in &batch.results {
        let said = match &r.outcome {
            FriendshipOutcome::Status(status) => status.as_str(),
            FriendshipOutcome::Error(e) => e,
        };
        out.push_str(&format!("{}: {said}\n", r.agent));
    }
    out.pop();
    out
}
