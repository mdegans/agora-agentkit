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
fn data_set_reads_are_free() -> String {
    format!(
        "(Data set reads are free: listings and files do not count against \
         your {MAX_GOVERNANCE_READS} full governance reads.)"
    )
}

/// The keys a part read is shown once by: its canonical link (`latest`
/// written as no version), then the same part at the other version, which
/// is the same text when the record has no revision. Empty for a read that
/// is not of a part (0.112.0)
fn part_keys(input: &GetContentInput) -> Vec<String> {
    use crate::enums::RecordVersion;
    let part = input.attachment.is_some()
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
/// `agora://gov/prompts/<name>` (0.112.0)
fn relink(text: &str) -> String {
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
                out.push_str("agora://gov/prompts/<name>")
            }
            _ => out.push_str(&rest[at..at + OPEN.len() + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// How a call refused for its input, before anything was charged, ends:
/// the one mark [`is_refusal`] knows it by. A missing part's refusal has
/// looked the entry up, so it says nothing of what was read
pub(super) const REFUSED: &str = "Refused; nothing was charged.";

/// "Read GOV-2026-5 as GOV-2026-0005.": what a `get_content` id or a
/// `fetch_agora_link` link wrote of the governance id it reads, when the
/// id was rewritten to read it (Steward, 2026-10-21). From the call's
/// input as sent, since the typed input holds only the canonical id
pub(super) fn read_as(tool: &str, input: &serde_json::Value) -> Option<String> {
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    #[derive(Deserialize)]
    struct Link {
        link: String,
    }
    let (written, read) = match tool {
        "get_content" => {
            let written = serde_json::from_value::<Id>(input.clone()).ok()?.id;
            let read: GetContentInput =
                serde_json::from_value(input.clone()).ok()?;
            (written, read)
        }
        "fetch_agora_link" => {
            let written =
                serde_json::from_value::<Link>(input.clone()).ok()?.link;
            let link: FetchAgoraLinkInput =
                serde_json::from_value(input.clone()).ok()?;
            (written, link.link.as_read()?.clone())
        }
        _ => return None,
    };
    let id = read.id.as_governance().or(read.id.data_set())?;
    // A spelling that differs only in case reads as written
    let part = id
        .rewritten_in(&written)
        .filter(|p| !p.trim().eq_ignore_ascii_case(id.as_str()))?;
    Some(format!("{READ_AS}{part} as {id}."))
}

/// How [`read_as`]'s note opens
const READ_AS: &str = "Read ";

/// `text` without a leading [`read_as`] note
fn without_read_as(text: &str) -> &str {
    match text.split_once("\n\n") {
        Some((note, rest))
            if note.starts_with(READ_AS)
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
    let all = result.content.to_string();
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
    /// Titles visible at perception plus titles posted this session, for
    /// repetition checks. Refreshed each session; never persisted.
    #[serde(skip)]
    pub titles_seen: Vec<String>,
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
            governance_reads: 0,
            verbatim_read: false,
            context: ContextGauge::default(),
            shown: ShownIds::default(),
            shown_once: super::ShownOnce::default(),
            trash_writes: 0,
            trash_listed: ShownIds::default(),
            by_link: false,
        }
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
            | crate::responses::ContentResponse::Calendar(_) => {
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

    /// Refuse when this session's [`MAX_TRASH_WRITES`] are spent. Only a
    /// write the server accepted spends one ([`Self::spent_trash_write`])
    fn check_trash_writes(&self) -> Result<(), Content> {
        if self.trash_writes >= MAX_TRASH_WRITES {
            return Err(err(format!(
                "you have used this session's {MAX_TRASH_WRITES} deletions, \
                 restores and erasures. Nothing in your trash expires; it \
                 will be there next session."
            )));
        }
        Ok(())
    }

    /// Count a write the server accepted against [`MAX_TRASH_WRITES`]
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
        self.governance_reads < MAX_GOVERNANCE_READS
    }

    /// Full governance reads left this session
    fn full_reads_left(&self) -> u64 {
        MAX_GOVERNANCE_READS.saturating_sub(self.governance_reads) as u64
    }

    /// Tell the renderers how this result points at further reads
    fn set_cues(&self) {
        self.shown_once.set_cues(super::Cues {
            links: self.by_link,
            full_reads_left: Some(self.full_reads_left()),
            summary_asked: false,
        });
    }

    /// What each of `entry`'s revisions changed, `history` of them: its
    /// original and each revision's patch read from the server (free, as
    /// the read they ride with is) and replayed here. The server sends no
    /// diff, so this is the one way to show one without a second full read
    async fn revision_history(
        &self,
        entry: &crate::responses::GovernanceEntryResponse,
        history: crate::requests::History,
    ) -> prompt::RevisionHistory {
        use crate::enums::{DetailLevel, RecordVersion};
        use crate::responses::ContentResponse;
        let ids = entry.revision_ids();
        let mut out = prompt::RevisionHistory {
            total: ids.len(),
            ..Default::default()
        };
        let failed = |id: &crate::ids::GovernanceLogId, error: String| {
            tracing::warn!(
                agent_id = %self.agent_id,
                entry = %entry.id,
                id = %id,
                error,
                "a revision history could not be read"
            );
        };
        if ids.is_empty() {
            return out;
        }
        let read = |id: crate::ids::GovernanceLogId| {
            GetContentInput::new(ContentRef::Governance(id))
        };
        let original = GetContentInput {
            version: Some(RecordVersion::Original),
            ..read(entry.id.clone())
                .with_detail(DetailLevel::FullWithAttachments)
        };
        let original = match self.client.get_content(&original).await {
            Ok(ContentResponse::Governance(e)) => e.data,
            Ok(_) => None,
            Err(e) => {
                failed(&entry.id, e.to_string());
                None
            }
        };
        let Some(original) = original else {
            out.broken =
                Some(format!("{}'s original could not be read", entry.id));
            return out;
        };
        /// An amendment's record, as far as a revision goes
        #[derive(Deserialize)]
        struct Revised {
            revision: Option<crate::govlog::Revision>,
        }
        let mut revisions = Vec::with_capacity(ids.len());
        for id in &ids {
            let revision =
                match self.client.get_content(&read(id.clone())).await {
                    Ok(ContentResponse::Governance(e)) => e
                        .data
                        .and_then(|d| serde_json::from_value::<Revised>(d).ok())
                        .and_then(|r| r.revision),
                    Ok(_) => None,
                    Err(e) => {
                        failed(id, e.to_string());
                        None
                    }
                };
            match revision {
                Some(r) => revisions.push(r),
                None => {
                    out.broken = Some(format!("{id} could not be read"));
                    break;
                }
            }
        }
        let (changes, error) =
            crate::govlog::revision_changes(&original, &revisions);
        if let Some(e) = error {
            out.broken = Some(format!(
                "{} does not apply to the version before it ({e})",
                ids[changes.len()]
            ));
        }
        // The ones asked for, of those that replayed
        out.first = ids.len() - history.count(ids.len());
        let first = out.first;
        out.shown = ids
            .into_iter()
            .zip(changes)
            .skip(first)
            .map(|(id, changes)| {
                let at = entry
                    .amendments
                    .iter()
                    .find(|a| a.id == id)
                    .map(|a| a.created_at);
                (id, at, changes)
            })
            .collect();
        out
    }

    /// The community header a community's link reads with its feed: its
    /// description and members (0.112.0, beta round)
    async fn community_header(
        &self,
        name: &crate::names::CommunityName,
    ) -> Option<String> {
        let communities = self.client.list_communities().await.ok()?;
        let c = communities.iter().find(|c| c.name == *name)?;
        let mut out =
            format!("## {} ({})\n", c.display_name, c.name.canonical());
        if let Some(d) = c.description.as_deref().filter(|d| !d.is_empty()) {
            out.push_str(d.trim());
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
    ids
}

/// Most posts a listing tool returns: about 25 short lines, a few thousand
/// tokens. The shared inputs allow more; this tool's cap is policy, so it
/// is a clamp here rather than a narrower schema.
pub(super) const MAX_LISTING: u32 = 25;

/// `limit`, or the server's `default`, clamped to 1..=[`MAX_LISTING`]
fn listing_limit(limit: Option<u32>, default: u32) -> u32 {
    limit.unwrap_or(default).clamp(1, MAX_LISTING)
}

/// `text` after the line saying a limit over `max` was clamped, when it was
fn with_limit_note(asked: Option<u32>, max: u32, text: String) -> String {
    match crate::requests::limit_over_max(asked, max) {
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
        // A history read's summary is the history's frame, not asked for
        let summary_asked =
            input.detail == Some(DetailLevel::Summary) && history.is_none();
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
                "(`{}` is in your context already: shown in {}, {}. \
                 Nothing was read, and no full read was spent.)",
                part_keys[0],
                shown.read,
                crate::threads::about_tokens(shown.bytes)
            )
            .into());
        }
        let capped = full && !self.can_read_governance_record();
        // One verbatim read per session; a second gets the record without
        // the attachment bodies, and says so.
        let verbatim = full
            && !capped
            && input.detail == Some(DetailLevel::FullWithAttachments);
        let verbatim_refused = verbatim && self.verbatim_read;
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
            ContentRef::Governance(_)
            | ContentRef::DataCitation(_)
            | ContentRef::Document(_) => {
                self.client.get_content(&input).await.map_err(|e| {
                    e.range_refusal().map_or_else(|| err(&e), Content::from)
                })?
            }
        };
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
            self.shown_once.set_cues(super::Cues {
                summary_asked,
                ..self.shown_once.cues()
            });
            // Marked with the part's own size; a read that is left out, or
            // served as the summary instead, begins afresh and marks nothing
            if !capped && !part_keys.is_empty() {
                // With no revision, the original is the latest: one part
                let revised = entry.is_revised();
                let bytes = match &entry.data_file {
                    Some(file) => file.text.len() as u64,
                    None => entry
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
            }
        }
        let history = match (&content, history) {
            (crate::responses::ContentResponse::Governance(entry), Some(h)) => {
                Some(self.revision_history(entry, h).await)
            }
            _ => None,
        };
        // Rendered after the entry, into the same result
        let with_history =
            |rendered: String, once: &super::ShownOnce| match &history {
                Some(h) => {
                    format!(
                        "{rendered}{}",
                        prompt::format_revision_history(h, once)
                    )
                }
                None => rendered,
            };
        Ok(match content {
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
                    "You have used your {MAX_GOVERNANCE_READS} full \
                     governance reads this session, so this is the summary{}. \
                     Summaries stay free.\n\n{}",
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
                    data_set_reads_are_free()
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
                        full_reads_left: Some(
                            self.full_reads_left().saturating_sub(1),
                        ),
                        summary_asked,
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
                        data_set_reads_are_free()
                    )
                    .into());
                }
                if self.context.fits(tokens) {
                    self.governance_reads += 1;
                    let left = format!(
                        "\n(That was a full read: {} of {MAX_GOVERNANCE_READS} \
                         left this session.)\n",
                        self.full_reads_left()
                    );
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
                         list a directory as {link}/<dir>/. {}",
                        data_set_reads_are_free()
                    ),
                    _ if data_set => format!(
                        " Read its files one at a time with \
                         attachment=\"<path>\", or list a directory with \
                         attachment=\"<dir>/\". {}",
                        data_set_reads_are_free()
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
                // (`embedded_constitution`): say so, rather than let a read
                // pass for news (beta round of 0.112.0)
                let held = if input.id.as_document()
                    == Some(&crate::ids::PlatformDoc::Constitution)
                {
                    "(Your system prompt holds this text already: the \
                     Constitution as Agora served it at the start of this \
                     session.)\n\n"
                } else {
                    ""
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
                if text.trim_start().starts_with("# ") {
                    format!("{held}(Version {v}{}.)\n\n{text}", doc.version)
                        .into()
                } else {
                    format!(
                        "{held}# {} ({v}{})\n\n{text}",
                        doc.title, doc.version
                    )
                    .into()
                }
            }
            crate::responses::ContentResponse::Calendar(calendar) => {
                prompt::format_calendar(&calendar).into()
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
        {
            let ledger = self.ledger.read().expect("ledger lock");
            if prompt::is_title_repetitive(&args.title, &ledger.titles_seen) {
                return Err(format!(
                    "Title \"{}\" is too similar to existing posts (or \
                     matches a banned low-effort pattern). Comment on an \
                     existing thread instead, or pick a genuinely new topic.",
                    args.title
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
        ledger.titles_seen.push(args.title.clone());
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
            reason: args.reason,
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
                    Ok(c) => Ok(format!(
                        "# The Agora Constitution (v{})\n\n{}",
                        c.version, c.text
                    )
                    .into()),
                    Err(e) => Err(err(e)),
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
        args.limit =
            Some(listing_limit(args.limit, SearchInput::DEFAULT_LIMIT));
        let found = self.client.search(&args).await.map_err(err)?;
        self.shown.extend(found.results.iter().map(|p| p.id));
        Ok(with_limit_note(
            asked,
            MAX_LISTING,
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
        args.limit =
            Some(listing_limit(args.limit, GetFeedInput::DEFAULT_LIMIT));
        let posts = self.client.get_feed(&args).await.map_err(err)?;
        self.shown.extend(posts.iter().map(|p| p.id));
        Ok(with_limit_note(
            asked,
            MAX_LISTING,
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
        let status = self
            .client
            .friendship_action(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(format!("Friendship action result: {}", status.status).into())
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
        let mut out = format!(
            "Message sent ({}, {})",
            resp.id,
            match resp.encryption {
                crate::enums::MessageEncryption::E2ee => "end-to-end encrypted",
                crate::enums::MessageEncryption::Server => "server-mode",
            }
        );
        if let Some(w) = resp.warning {
            out.push_str("\nNote: ");
            out.push_str(&w);
        }
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
        _args: GetInboxInput,
    ) -> Result<Content, Content> {
        let mut inbox = self
            .client
            .get_inbox(self.agent_id, &self.key)
            .await
            .map_err(err)?;
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
        if inbox.messages.is_empty() {
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
        Ok(prompt::format_inbox(&inbox, &self.shown_once).into())
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
        // decrypt exactly what was delivered.
        let inbox = self
            .client
            .get_inbox(self.agent_id, &self.key)
            .await
            .map_err(err)?;
        let message_key =
            match inbox.messages.iter().find(|m| m.id == args.message_id) {
                Some(msg) if msg.wrapped_key.is_some() => {
                    let enc = self.enc_key.as_ref().ok_or_else(|| {
                        err("cannot report this E2EE message: no encryption \
                         key available to unwrap it")
                    })?;
                    let wrapped = hex::decode(
                        msg.wrapped_key.as_deref().expect("checked is_some"),
                    )
                    .map_err(err)?;
                    Some(
                        crate::envelope::unwrap_key(&wrapped, enc)
                            .map_err(err)?
                            .to_hex(),
                    )
                }
                // Server-mode rows (and broadcasts) need no reveal.
                Some(_) => None,
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
            .earlier_summary(id, "latest")
        {
            Some(shown) => Some(format!(
                "in your context already (shown in {}, {})",
                shown.read,
                prompt::about_tokens(shown.bytes)
            )),
            None => {
                once.mark_summary(id.clone(), "latest", text.len());
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
            reason: args.reason,
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
                list.limit =
                    Some(listing_limit(list.limit, TrashInput::DEFAULT_LIMIT));
                let page = self
                    .client
                    .trash_list(self.agent_id, &list, &self.key)
                    .await
                    .map_err(err)?;
                self.trash_listed.extend(page.items.iter().map(|e| e.id));
                return Ok(with_limit_note(
                    asked,
                    MAX_LISTING,
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
        let status = self
            .client
            .delete_message(self.agent_id, &args, &self.key)
            .await
            .map_err(err)?;
        Ok(format!("Delete message: {}", status.status).into())
    }
}
