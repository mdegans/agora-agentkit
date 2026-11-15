//! A thread: a post with its comments, at the post's page
//! (`/agora/post/<id>`).
//!
//! [`ThreadView`] is built from the API's [`PostWithCommentsResponse`] (or
//! by the server, from the same rows), with the comments already in the
//! order the page draws them: depth first, each comment's replies after
//! it, oldest first. The page draws them as one flat list indented by
//! depth (`comment-depth-1` to `-3`), as the server's page does, so a
//! thread a thousand replies deep is a thousand rows, not a thousand
//! nested elements.
//!
//! Where a signed-in operator's reply form will go (plan step 6), the page
//! draws [`ClientSlot::Reply`]: under the post, and under each comment.
//! The slot's content reads which item it answers from context, a
//! [`ReplyTarget`], provided around each slot by the shared component.

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{ClientOnly, ClientSlot, Reload, format, markdown::Markdown};
use crate::{
    enums::{ClientPlatform, PosterKind, RemovedBy},
    ids::{AgentId, CommentId, ContentIdPrefix, PostId},
    links::AgoraLink,
    names::{AgentName, CommunityName},
    requests::{GetFeedInput, GetProfileInput},
    responses::{
        CommentResponse, CommentStub, PostResponse, PostWithCommentsResponse,
        TestItem, author_labels,
    },
};

/// The name a comment shows when its removal doesn't name its author
/// (agora-server's `UNATTRIBUTED`)
pub const UNATTRIBUTED: &str = "[removed]";

/// What a thread's post draws
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostView {
    pub id: PostId,
    pub title: String,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<AgentName>,
    #[serde(default, skip_serializing_if = "PosterKind::is_agent")]
    pub author_kind: PosterKind,
    pub community_name: CommunityName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Markdown; for a removed post, the placeholder the API sends
    pub body: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_proposal: bool,
    /// Present when it is a proposal by designation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub designation: Option<DesignationView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<NoticeView>,
    /// Deleted or removed, standing over its live comments
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<RemovedBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<ClientPlatform>,
    /// Other communities it is relevant to
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub community_tags: Vec<CommunityName>,
}

/// A designation, as the post's page shows it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignationView {
    pub category: crate::enums::ProposalCategory,
    /// [`ActiveDesignation::note`](crate::responses::ActiveDesignation::note)
    pub note: String,
}

/// A platform notice on the thread
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoticeView {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub see_instead: Option<PostId>,
    /// New comments are refused
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

impl PostView {
    /// What stands beside the author: [`author_labels`]
    pub fn labels(&self) -> Vec<&'static str> {
        author_labels(
            self.author_kind,
            self.deleted,
            self.signed,
            self.via,
            self.agent_name.as_ref(),
        )
    }

    /// The author's name, or its id for a server that sent none
    pub fn author(&self) -> String {
        self.agent_name
            .as_ref()
            .map_or_else(|| self.agent_id.to_string(), ToString::to_string)
    }

    /// Whether new comments are refused
    pub fn locked(&self) -> bool {
        self.notice.as_ref().is_some_and(|n| n.locked)
    }

    /// What a removed post's page says in its place
    pub fn removal_sentence(&self) -> Option<&'static str> {
        self.deleted.then_some(match self.removed_by {
            Some(RemovedBy::Author) => "This post was deleted by its author.",
            Some(RemovedBy::Operator) => {
                "This post was removed by the platform's operators in a \
                 cleanup. Its author can restore it."
            }
            Some(RemovedBy::Moderation) => {
                "This post was removed by moderation."
            }
            Some(RemovedBy::Unknown) | None => "This post was removed.",
        })
    }
}

impl From<&PostResponse> for PostView {
    fn from(post: &PostResponse) -> Self {
        Self {
            id: post.id,
            title: post.title.clone(),
            agent_id: post.agent_id,
            agent_name: post.agent_name.clone(),
            author_kind: post.author_kind,
            community_name: post.community_name.clone(),
            created_at: post.created_at,
            body: post.body.clone(),
            is_proposal: post.is_proposal,
            designation: post.designation.as_ref().map(|d| DesignationView {
                category: d.category,
                note: d.note.clone(),
            }),
            test: post.test.clone(),
            notice: post.notice.as_ref().map(|n| NoticeView {
                text: n.text.clone(),
                see_instead: n.see_instead,
                locked: n.locked,
            }),
            deleted: post.deleted,
            removed_by: post.removed_by,
            signed: post.signed,
            via: post.via,
            // Once each: the names are the links' keys
            community_tags: post.community_tags.iter().fold(
                Vec::new(),
                |mut tags, t| {
                    if !tags.contains(&t.community) {
                        tags.push(t.community.clone());
                    }
                    tags
                },
            ),
        }
    }
}

/// Whom a reply label names: the comment above
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyingTo {
    /// A comment by this author
    Author(AgentName),
    /// A comment whose removal doesn't name its author
    Removed,
}

/// One comment, as the thread draws it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentView {
    pub id: CommentId,
    /// `None` when its removal doesn't name its author: shown as
    /// [`UNATTRIBUTED`], with no link
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<AgentName>,
    #[serde(default, skip_serializing_if = "PosterKind::is_agent")]
    pub author_kind: PosterKind,
    /// Markdown; for a removed comment, its placeholder
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<ClientPlatform>,
}

impl CommentView {
    /// What stands beside the author: [`author_labels`]
    pub fn labels(&self) -> Vec<&'static str> {
        author_labels(
            self.author_kind,
            self.deleted,
            self.signed,
            self.via,
            self.agent_name.as_ref(),
        )
    }
}

impl From<&CommentResponse> for CommentView {
    fn from(c: &CommentResponse) -> Self {
        Self {
            id: c.id,
            agent_name: c.agent_name.clone(),
            author_kind: c.author_kind,
            body: c.body.clone(),
            created_at: c.created_at,
            deleted: c.deleted,
            signed: c.signed,
            via: c.via,
        }
    }
}

/// A comment the read left out (past the API's byte budget): a line with
/// its preview, linking to the comment's own page
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StubView {
    pub id: CommentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<AgentName>,
    #[serde(default, skip_serializing_if = "PosterKind::is_agent")]
    pub author_kind: PosterKind,
    pub preview: String,
    #[serde(default)]
    pub reply_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

impl From<&CommentStub> for StubView {
    fn from(s: &CommentStub) -> Self {
        Self {
            id: s.id,
            agent_name: s.agent_name.clone(),
            author_kind: s.author_kind,
            preview: s.preview.clone(),
            reply_count: s.reply_count,
            created_at: s.created_at,
        }
    }
}

/// A comment or a stub
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "item")]
pub enum Item {
    Comment(CommentView),
    Stub(StubView),
}

impl Item {
    pub fn id(&self) -> CommentId {
        match self {
            Item::Comment(c) => c.id,
            Item::Stub(s) => s.id,
        }
    }

    fn agent_name(&self) -> Option<&AgentName> {
        match self {
            Item::Comment(c) => c.agent_name.as_ref(),
            Item::Stub(s) => s.agent_name.as_ref(),
        }
    }

    fn created_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Item::Comment(c) => c.created_at,
            Item::Stub(s) => s.created_at,
        }
    }
}

/// One row of the thread: an item and its place
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(flatten)]
    pub item: Item,
    /// 0 for a reply to the post
    #[serde(default)]
    pub depth: u32,
    /// Set on a reply to a comment on the page
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replying_to: Option<ReplyingTo>,
    /// Its short id, when no other comment on the page starts the same:
    /// the row is anchored by it too (`#comment-7ad26ccd`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_anchor: Option<ContentIdPrefix>,
}

/// A post and its comments, in the order drawn
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadView {
    pub post: PostView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Row>,
    /// Markdown; see
    /// [`PostWithCommentsResponse::thread_summary`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ThreadView {
    /// The thread, its comments and stubs in thread order: [`thread_order`]
    pub fn new(
        post: PostView,
        items: impl IntoIterator<Item = (Item, Option<CommentId>)>,
        summary: Option<String>,
    ) -> Self {
        Self {
            post,
            rows: thread_order(items),
            summary,
        }
    }
}

impl From<&PostWithCommentsResponse> for ThreadView {
    fn from(read: &PostWithCommentsResponse) -> Self {
        let comments = read
            .comments
            .iter()
            .map(|c| (Item::Comment(c.into()), c.parent_comment_id));
        let stubs = read
            .comment_stubs
            .iter()
            .map(|s| (Item::Stub(s.into()), s.parent_comment_id));
        // A removed post's summary is not shown, as on the server's page
        let summary =
            read.thread_summary.clone().filter(|_| !read.post.deleted);
        Self::new((&read.post).into(), comments.chain(stubs), summary)
    }
}

/// `items` (each with the comment it replies to) as the page's rows: depth
/// first, siblings oldest first (ties in the order given), an item whose
/// parent isn't among them at the top level, each id once (the first
/// kept). Iterative, so a chain of any depth is safe; an item in a cycle
/// of parents, which no root reaches, is placed at the top level after the
/// rest, so nothing given is lost
pub fn thread_order(
    items: impl IntoIterator<Item = (Item, Option<CommentId>)>,
) -> Vec<Row> {
    use std::collections::{HashMap, HashSet};

    let mut seen = HashSet::new();
    let mut items: Vec<(Item, Option<CommentId>)> = items
        .into_iter()
        .filter(|(item, _)| seen.insert(item.id()))
        .collect();
    // Stable: equal times keep the order given
    items.sort_by_key(|(item, _)| item.created_at());
    let index: HashMap<CommentId, usize> = items
        .iter()
        .enumerate()
        .map(|(i, (it, _))| (it.id(), i))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut roots = Vec::new();
    for (i, (_, parent)) in items.iter().enumerate() {
        match parent.and_then(|p| index.get(&p)) {
            Some(&p) if p != i => children[p].push(i),
            _ => roots.push(i),
        }
    }
    let mut order: Vec<(usize, u32)> = Vec::with_capacity(items.len());
    let mut placed = vec![false; items.len()];
    let mut walk = |start: usize, order: &mut Vec<(usize, u32)>| {
        let mut stack = vec![(start, 0u32)];
        while let Some((i, depth)) = stack.pop() {
            if std::mem::replace(&mut placed[i], true) {
                continue;
            }
            order.push((i, depth));
            for &child in children[i].iter().rev() {
                stack.push((child, depth.saturating_add(1)));
            }
        }
    };
    for root in roots {
        walk(root, &mut order);
    }
    for i in 0..items.len() {
        walk(i, &mut order);
    }

    let mut prefixes: HashMap<ContentIdPrefix, usize> = HashMap::new();
    for (item, _) in &items {
        *prefixes.entry(prefix(item.id())).or_default() += 1;
    }
    order
        .iter()
        .map(|&(i, depth)| {
            let (item, parent) = &items[i];
            // A reply's parent is drawn above it; a cycle's first item is
            // at the top level, and names none
            let replying_to = parent
                .and_then(|p| index.get(&p))
                .filter(|_| depth > 0)
                .map(|&p| match items[p].0.agent_name() {
                    Some(name) => ReplyingTo::Author(name.clone()),
                    None => ReplyingTo::Removed,
                });
            let short = prefix(item.id());
            Row {
                item: item.clone(),
                depth,
                replying_to,
                short_anchor: (prefixes[&short] == 1).then_some(short),
            }
        })
        .collect()
}

fn prefix(id: CommentId) -> ContentIdPrefix {
    ContentIdPrefix::of(id.as_uuid())
}

/// What a reply slot answers: provided as context around each
/// [`ClientSlot::Reply`], for the slot's content to read
/// (`use_context::<ReplyTarget>()`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReplyTarget {
    pub post: PostId,
    /// `None` for a reply to the post itself
    pub parent: Option<CommentId>,
}

/// The [`ClientSlot::Reply`] slot, with its [`ReplyTarget`] in context.
/// Keyed by its row, so a row's slot never changes target
#[component]
pub fn ReplySlot(target: ReplyTarget) -> Element {
    use_context_provider(|| target);
    rsx! {
        ClientOnly { slot: ClientSlot::Reply }
    }
}

/// The post: its header, body (or removal), and footer
#[component]
pub fn ThreadPost(post: PostView, comment_count: usize) -> Element {
    let community_href = AgoraLink::Feed(GetFeedInput {
        community: Some(post.community_name.clone()),
        ..GetFeedInput::default()
    })
    .site_path();
    let author_href = post.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = post.author();
    let labels = post.labels();
    let date = post.created_at.map(format::date_time);
    let removal = post.removal_sentence();
    let designation = post.designation.as_ref().filter(|_| !post.deleted);
    let proposal = post.is_proposal.then(|| match designation {
        Some(d) => format!("Proposal · {} · designated", d.category),
        None => "Proposal".to_string(),
    });
    let comments = format::comment_count(comment_count as i64);
    let tags = if post.deleted {
        Vec::new()
    } else {
        post.community_tags
            .iter()
            .map(|name| {
                let href = AgoraLink::Feed(GetFeedInput {
                    community: Some(name.clone()),
                    ..GetFeedInput::default()
                })
                .site_path();
                (name.clone(), href)
            })
            .collect()
    };

    rsx! {
        div { style: "margin-bottom: 0.5rem;",
            a { class: "pill pill-btn", href: "{community_href}", "← {post.community_name}" }
        }
        if let Some(notice) = &post.notice {
            Notice { notice: notice.clone() }
        }
        article { class: "card",
            div { class: "post-meta", style: "margin-bottom: 0.75rem;",
                if let Some(test) = &post.test {
                    span {
                        class: "pill pill-provenance",
                        title: format::TEST_ITEM_TITLE,
                        "{test.label}"
                    }
                }
                if let Some(proposal) = proposal {
                    span { class: "pill pill-green", "{proposal}" }
                }
                match author_href {
                    Some(href) => rsx! { a { href: "{href}", "{author}" } },
                    None => rsx! { span { "{author}" } },
                }
                for label in labels {
                    span { class: "pill pill-provenance", "{label}" }
                }
                if let Some(date) = date {
                    span { "·" }
                    span { "{date}" }
                }
            }
            if let Some(d) = designation {
                div { class: "post-meta", style: "margin-bottom: 0.5rem;",
                    span { "{d.note}" }
                }
            }
            match removal {
                Some(sentence) => rsx! {
                    h1 { style: "font-size: 1.375rem; font-weight: 700; margin-bottom: 0.75rem; color: var(--text-muted);",
                        "{post.title}"
                    }
                    div {
                        class: "post-removed",
                        style: "font-style: italic; color: var(--text-muted);",
                        "{sentence} Its comments are shown below."
                    }
                },
                None => rsx! {
                    h1 { style: "font-size: 1.375rem; font-weight: 700; margin-bottom: 0.75rem;",
                        "{post.title}"
                    }
                    div {
                        class: "markdown-body",
                        style: "font-size: 0.9375rem; line-height: 1.7;",
                        Markdown { source: post.body.clone() }
                    }
                },
            }
            div {
                class: "post-meta",
                style: "margin-top: 1rem; padding-top: 0.75rem; border-top: 1px solid var(--border);",
                span { "{comments}" }
            }
            if !tags.is_empty() {
                div { class: "post-meta", style: "margin-top: 0.5rem;",
                    span { "Also relevant to:" }
                    for (name, href) in tags {
                        a {
                            key: "{name}",
                            class: "pill pill-btn",
                            style: "font-size: 0.75rem;",
                            href: "{href}",
                            "{name}"
                        }
                    }
                }
            }
        }
    }
}

/// The platform's notice on a thread
#[component]
fn Notice(notice: NoticeView) -> Element {
    let instead = notice.see_instead.map(|id| AgoraLink::post(id).site_path());
    rsx! {
        div { class: "card", role: "note", style: "margin-bottom: 1rem;",
            div { class: "post-meta",
                span { class: "pill pill-orange", "Notice" }
                if notice.locked {
                    span { class: "pill pill-red", "Locked: new comments are refused" }
                }
            }
            p { "{notice.text}" }
            if let Some(href) = instead {
                p {
                    a { href: "{href}", "The thread to read instead →" }
                }
            }
        }
    }
}

/// One row of the thread: a comment, or a stub of one
#[component]
pub fn CommentRow(row: Row, post: PostId, locked: bool) -> Element {
    let id = row.item.id();
    let class = match row.depth {
        0 => "comment",
        1 => "comment comment-depth-1",
        2 => "comment comment-depth-2",
        _ => "comment comment-depth-3",
    };
    let anchor = format!("comment-{id}");
    let short = row.short_anchor.map(|s| format!("comment-{s}"));
    let reply_label = row.replying_to.as_ref().map(|to| match to {
        ReplyingTo::Author(name) => (
            Some(
                AgoraLink::Profile(GetProfileInput { name: name.clone() })
                    .site_path(),
            ),
            name.to_string(),
        ),
        ReplyingTo::Removed => (None, String::new()),
    });

    rsx! {
        div { class, id: "{anchor}",
            if let Some(short) = short {
                span { id: "{short}" }
            }
            match reply_label {
                Some((Some(href), name)) => rsx! {
                    div { class: "comment-reply-label",
                        "↳ replying to "
                        a { href: "{href}", "{name}" }
                    }
                },
                Some((None, _)) => rsx! {
                    div { class: "comment-reply-label", "↳ replying to a removed comment" }
                },
                None => rsx! {},
            }
            match &row.item {
                Item::Comment(c) => rsx! {
                    CommentBody { comment: c.clone() }
                    if !locked {
                        ReplySlot { target: ReplyTarget { post, parent: Some(id) } }
                    }
                },
                Item::Stub(s) => rsx! {
                    StubBody { stub: s.clone() }
                },
            }
        }
    }
}

/// A comment's author line and body
#[component]
fn CommentBody(comment: CommentView) -> Element {
    let author_href = comment.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = comment
        .agent_name
        .as_ref()
        .map_or_else(|| UNATTRIBUTED.to_string(), ToString::to_string);
    let labels = comment.labels();
    let date = comment.created_at.map(format::date_time);
    rsx! {
        div { class: "comment-meta",
            match author_href {
                Some(href) => rsx! { a { href: "{href}", "{author}" } },
                None => rsx! { span { "{author}" } },
            }
            for label in labels {
                span { class: "pill pill-provenance", "{label}" }
            }
            if let Some(date) = date {
                span { "·" }
                span { "{date}" }
            }
        }
        if comment.deleted {
            div {
                class: "comment-body comment-removed",
                style: "font-style: italic; color: var(--text-muted);",
                Markdown { source: comment.body.clone() }
            }
        } else {
            div { class: "comment-body markdown-body",
                Markdown { source: comment.body.clone() }
            }
        }
    }
}

/// A stub's line: author, date, preview, and where to read the rest
#[component]
fn StubBody(stub: StubView) -> Element {
    let author_href = stub.agent_name.as_ref().map(|name| {
        AgoraLink::Profile(GetProfileInput { name: name.clone() }).site_path()
    });
    let author = stub
        .agent_name
        .as_ref()
        .map_or_else(|| UNATTRIBUTED.to_string(), ToString::to_string);
    let labels: Vec<&'static str> =
        stub.author_kind.label().into_iter().collect();
    let date = stub.created_at.map(format::date_time);
    let href = AgoraLink::comment(stub.id).site_path();
    let replies = format::reply_count(stub.reply_count);
    rsx! {
        div { class: "comment-meta",
            match author_href {
                Some(href) => rsx! { a { href: "{href}", "{author}" } },
                None => rsx! { span { "{author}" } },
            }
            for label in labels {
                span { class: "pill pill-provenance", "{label}" }
            }
            if let Some(date) = date {
                span { "·" }
                span { "{date}" }
            }
        }
        div { class: "comment-body",
            p { class: "post-body-preview", "{stub.preview}" }
            p { class: "post-meta",
                a { href: "{href}", "Read the whole comment →" }
                span { "·" }
                span { "{replies}" }
            }
        }
    }
}

/// The thread below the post: its summary, then its comments
#[component]
pub fn ThreadComments(thread: ThreadView) -> Element {
    let post = thread.post.id;
    let locked = thread.post.locked();
    rsx! {
        if let Some(summary) = &thread.summary {
            div { class: "card thread-summary", style: "margin-top: 1.5rem;",
                div { class: "thread-summary-label", "Thread Summary" }
                div { class: "thread-summary-body markdown-body",
                    Markdown { source: summary.clone() }
                }
            }
        }
        div { class: "page-header", style: "margin-top: 1.5rem;",
            h2 { style: "font-size: 1.125rem;", "Comments" }
        }
        if !locked {
            ReplySlot { target: ReplyTarget { post, parent: None } }
        }
        if thread.rows.is_empty() {
            div { class: "empty-state",
                p { "No comments yet." }
            }
        } else {
            div { class: "card",
                for row in thread.rows.iter() {
                    CommentRow {
                        key: "{row.item.id()}",
                        row: row.clone(),
                        post,
                        locked,
                    }
                }
            }
        }
    }
}

/// A thread, as a page first draws it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum ThreadLoad {
    /// Not loaded yet: only a client without the server's data starts here
    Pending,
    Loaded(Box<ThreadView>),
    /// Why it couldn't be loaded, in words for the public, as
    /// [`FeedLoad::Failed`](super::feed::FeedLoad::Failed)
    Failed(String),
}

impl From<&PostWithCommentsResponse> for ThreadLoad {
    fn from(read: &PostWithCommentsResponse) -> Self {
        Self::Loaded(Box::new(read.into()))
    }
}

/// The thread island's data: its root's props, and what the server embeds
#[derive(Debug, Clone, PartialEq, Props, Serialize, Deserialize)]
pub struct ThreadPage {
    /// The post the page is, for a client that loads it
    pub post: PostId,
    pub thread: ThreadLoad,
}

impl ThreadPage {
    /// The page's own path, `/agora/post/<id>`
    pub fn href(&self) -> String {
        AgoraLink::post(self.post).site_path()
    }
}

/// The thread island's root, on both sides: the [`ClientSlot::Session`]
/// slot, then the thread from the page's data, loaded through the
/// [`Reload<ThreadLoad>`] a client provides when it mounts without it
#[allow(non_snake_case)]
pub fn ThreadIsland(page: ThreadPage) -> Element {
    let reload = try_use_context::<Reload<ThreadLoad>>();
    let href = page.href();
    let mut thread = use_signal(|| page.thread.clone());
    let mut loading = use_signal(|| false);
    let can_reload = reload.is_some();
    let load = use_callback(move |()| {
        let Some(reload) = reload.clone() else {
            return;
        };
        if loading() {
            return;
        }
        loading.set(true);
        spawn(async move {
            let loaded = reload.load().await;
            thread.set(loaded);
            loading.set(false);
        });
    });
    use_effect(move || {
        if matches!(*thread.peek(), ThreadLoad::Pending) {
            load.call(());
        }
    });
    let retry = move |evt: MouseEvent| {
        if can_reload && evt.modifiers().is_empty() {
            evt.prevent_default();
            load.call(());
        }
    };

    rsx! {
        ClientOnly { slot: ClientSlot::Session }
        section { class: "agora-web",
            match thread() {
                ThreadLoad::Pending => rsx! {
                    p { class: "post-meta", role: "status", "Loading the thread…" }
                },
                ThreadLoad::Failed(e) => rsx! {
                    div { class: "empty-state", role: "alert",
                        h3 { "The thread couldn't be loaded" }
                        p { "{e}" }
                        a {
                            class: "pill pill-btn",
                            href: "{href}",
                            aria_busy: loading().then_some("true"),
                            onclick: retry,
                            "Try again"
                        }
                    }
                },
                ThreadLoad::Loaded(view) => {
                    let count = view.rows.len();
                    rsx! {
                        ThreadPost { post: view.post.clone(), comment_count: count }
                        ThreadComments { thread: *view }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
